//! # appa-eventlog — authoritative trajectory and workspace event streams
//!
//! This crate stores append-only histories and enforces compare-and-swap ordering for every
//! state transition they own. A root trajectory and its branches append to one shared log. It
//! holds two streams at one position: the engine's lasting facts, and the host's own observations —
//! what a harness saw of its inventory, its calls, its turns and the standing it recorded for them. The stored
//! policy files are immutable, content-addressed inputs. A canonical file workspace has its own
//! event stream because independent root families can share it. Everything else — a branch's
//! parent, whether it has ended, which dispatch is open, whether an offer still stands, file
//! versions, Labels, receipts and reservations — is projected by replaying the owning stream.
//!
//! This crate is where the log is written and read. The record encoding, the database, and the
//! conditional append are private to it: a caller hands it [`Fact`]s and gets [`Log`]s back, and
//! never names SQL, a row, or a byte. Where the log is kept is the closed [`Backend`] enum,
//! dispatched by `match`: SQLite for standalone use and optional PostgreSQL for
//! embedded hosts that install the schema through their own migrations.
//!
//! The durable structures are:
//!
//! - the log itself, one row per appended batch, keyed by the root trajectory;
//! - workspace file events, one row per appended transition, keyed by canonical workspace;
//! - the stored policy files, content addressed by the SHA-256 of their exact bytes, write-once
//!   and shared by every root that opened under them;
//! - the host keys, one row per (key, root) pair a host record ever named, written in the
//!   same transaction as the record that names it. This is the one derived table: it answers
//!   which families stand behind a key without a pass over every family's rows, and it cannot
//!   disagree with the log because a record and its key row commit or roll back together;
//! - file roots, a derived root-to-workspace lookup written in the same transaction as the
//!   authoritative `RootBound` workspace event;
//! - operations and processed results — typed receipts for idempotent claims, each pending one
//!   under the lease of the owner that claimed it. They are not engine facts. SQLite and Memory
//!   keep them beside the log; PostgreSQL hosts install the equivalent `openappa_*` tables
//!   through their own migrations;
//! - held peer messages — a message body one root holds for another until the receiver takes
//!   it. The body lives only there, never in the log.
//!
//! ### Storage Backend Scope & Retention
//!
//! Typed receipts (`operations`, `processed_results`) hold idempotent claim state across process
//! restarts. SQLite persists them in the daemon's database file; Memory holds them until the
//! store drops. PostgreSQL embedding hosts install the same contract as `openappa_operations`
//! and `openappa_processed_results`.
//!
//! There is no index from a branch to its root. Every caller already knows the root: a harness
//! event names it, and a surfaced offer's identity carries it. An index would be a third place
//! for the truth to live.
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

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub use appa_engine::fact::Fact;
use appa_engine::label::Label;
use appa_engine::profile::PolicyFileKey;
use appa_engine::value::DispatchId;
pub use appa_engine::value::TrajectoryId;
use appa_runtime_api::{
    AdapterName, LaunchToken, PeerAddress, PeerDigest, Ruling, SessionTitle, inventory::ToolInventory,
};

pub mod embedded;
mod encoding;
#[cfg(feature = "postgres")]
pub mod encryption;
pub mod files;
mod held;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod receipts;
mod sqlite;

use encoding::encode;
use sqlite::Sqlite;

use embedded::{DirectTake, EmbeddedClaim, EmbeddedError, EmbeddedRow, ReadTake};
pub use held::{HeldError, HeldNotice, HeldPeerId, HeldPeerMessage};
pub use receipts::{
    LeaseOwner, LeaseValueError, OperationClaim, OperationKey, OperationRequest, ProcessedResultClaim,
    ProcessedResultKey, ProcessedResultRequest, ReceiptBinding, ReceiptError, ReceiptLease, Renewal, SessionScope,
};

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
        /// How many connections the store may hold open at once. One opens with the store;
        /// the rest open as concurrent work asks for them.
        max_connections: std::num::NonZeroUsize,
    },
}

pub struct LogStore {
    store: Store,
    /// Shared with every store leased from this one, so a fail point armed here fires there.
    #[cfg(feature = "fault-injection")]
    faults: std::sync::Arc<FaultPoints>,
}

/// Where this store's log is kept. [`Backend::Memory`] is a SQLite connection to `:memory:`.
enum Store {
    Sqlite(Sqlite),
    #[cfg(feature = "postgres")]
    Postgres(postgres::PostgresStore),
}

#[cfg(feature = "fault-injection")]
#[derive(Default)]
struct FaultPoints {
    commits_until_failure: std::sync::atomic::AtomicU64,
    contended_appends: std::sync::atomic::AtomicU64,
    failing_reads: std::sync::atomic::AtomicU64,
    /// What the next foreign writer records rather than nothing, so a caller's re-derivation
    /// meets a changed state and not only a moved position.
    contending_record: std::sync::Mutex<Option<(TrajectoryId, TrajectoryId, HostObservation)>>,
}

/// The records of one read, and the position they were read at.
#[derive(Debug, Clone, PartialEq)]
pub struct Log {
    root: TrajectoryId,
    facts: Vec<Fact>,
    /// How many facts the log holds after each batch, so the facts of one batch can be told
    /// from the next.
    batch_ends: Vec<usize>,
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
        /// The host prompt a fan-out spawn was released under: its children name that prompt
        /// and no call, so this is what binds them to the spawn's fork.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// The host reported this bound call finished. A fan-out spawn's fork binds no new child
    /// after it.
    CallSettled { trajectory: TrajectoryId, call_id: String },
    /// The host's opaque identity for one call the engine denied, bound to the batch identity
    /// the denial was decided under. Written in the same batch as the denial's facts, so a
    /// repeat of the call — the delivery retry a restart produces — is answered from the
    /// recorded decision rather than decided again with fresh offer identities.
    CallDenied {
        trajectory: TrajectoryId,
        call_id: String,
        batch: String,
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
    /// This root receives peer messages at `address`.
    Addressed { address: PeerAddress },
    /// This root's host shows it under `title`.
    Titled { title: SessionTitle },
    /// This root sent the peer message whose body digests to `digest`, carrying `label`, from
    /// the dispatch the send ran under.
    PeerSent {
        digest: PeerDigest,
        label: Label,
        dispatch: DispatchId,
    },
    /// This root admitted the peer message whose body digests to `digest`.
    PeerAdmitted { digest: PeerDigest },
    /// This root is the family of the protected launch `launch`.
    Launched { launch: LaunchToken },
    /// The launch left this family for `to` when its host cleared the conversation. The
    /// family keeps its history and stops answering for the launch and its address.
    LaunchMoved { launch: LaunchToken, to: TrajectoryId },
    /// The host continues this family under `host`, another of its own session ids.
    Aliased { host: TrajectoryId },
}

impl HostObservation {
    /// The key this observation names, where it names one: a standing taken, held, or spent.
    /// The store writes it beside the record so [`LogStore::roots_mentioning`] can find the
    /// families that recorded it.
    pub fn key(&self) -> Option<String> {
        match self {
            Self::Vouched { key, .. } | Self::Claimed { key, .. } | Self::Released { key, .. } => Some(key.clone()),
            Self::Addressed { address } => Some(peer_address_key(address)),
            Self::Titled { title } => Some(peer_title_key(title)),
            Self::PeerSent { digest, .. } => Some(peer_sent_key(digest)),
            Self::Launched { launch } => Some(launch_key(launch)),
            Self::Aliased { host } => Some(host_alias_key(host)),
            Self::Inventory { .. }
            | Self::CallBound { .. }
            | Self::CallSettled { .. }
            | Self::CallDenied { .. }
            | Self::PromptSeen { .. }
            | Self::PromptSettled { .. }
            | Self::TurnEnded { .. }
            | Self::PeerAdmitted { .. }
            | Self::LaunchMoved { .. } => None,
        }
    }
}

/// The key a [`HostObservation::Addressed`] record names, for [`LogStore::roots_mentioning`].
pub fn peer_address_key(address: &PeerAddress) -> String {
    format!("peer-address:{address}")
}

/// The key a [`HostObservation::Launched`] record names.
pub fn launch_key(launch: &LaunchToken) -> String {
    format!("launch:{launch}")
}

/// The key a [`HostObservation::Aliased`] record names.
pub fn host_alias_key(host: &TrajectoryId) -> String {
    format!("host-alias:{}", host.0)
}

/// The key a [`HostObservation::Titled`] record names.
pub fn peer_title_key(title: &SessionTitle) -> String {
    format!("peer-title:{title}")
}

/// The key a [`HostObservation::PeerSent`] record names.
pub fn peer_sent_key(digest: &PeerDigest) -> String {
    format!("peer-sent:{digest}")
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
    pub prompt: Option<&'a str>,
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

    /// The facts of every batch after position `basis`, one slice per batch in log order.
    /// `None` when this read stands before `basis`.
    pub fn batches_after(&self, basis: u64) -> Option<impl Iterator<Item = &[Fact]>> {
        let first = usize::try_from(basis)
            .ok()
            .filter(|first| *first <= self.batch_ends.len())?;
        let start = match first {
            0 => 0,
            first => self.batch_ends[first - 1],
        };
        let ends = &self.batch_ends[first..];
        Some(ends.iter().scan(start, |start, end| {
            let batch = &self.facts[*start..*end];
            *start = *end;
            Some(batch)
        }))
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
                prompt,
            } => Some(CallBinding {
                trajectory,
                call_id,
                dispatch,
                prompt: prompt.as_deref(),
            }),
            _ => None,
        })
    }

    /// The batch identity a denied call was decided under, so its repeat is answered from
    /// the record. The latest record wins: a call id denied again after the state moved
    /// names the denial that still stands.
    pub fn denied_call_batch(&self, trajectory: &TrajectoryId, call_id: &str) -> Option<&str> {
        self.host.iter().rev().find_map(|record| match &record.observation {
            HostObservation::CallDenied {
                trajectory: owner,
                call_id: id,
                batch,
            } if owner == trajectory && id == call_id => Some(batch.as_str()),
            _ => None,
        })
    }

    /// The bound calls the host reported finished, as (trajectory, call id).
    pub fn settled_calls(&self) -> std::collections::BTreeSet<(&TrajectoryId, &str)> {
        self.host
            .iter()
            .filter_map(|record| match &record.observation {
                HostObservation::CallSettled { trajectory, call_id } => Some((trajectory, call_id.as_str())),
                _ => None,
            })
            .collect()
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
    /// The root's log is in an archived database.
    Archived,
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
            CreateError::Archived { .. } => StoreErrorClass::Archived,
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
            AppendError::ReadClaimLost => StoreErrorClass::Conflict,
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
    #[error(
        "the database at {path} was written by a newer appa (schema version {found}; this build reads up to {supported}); upgrade appa"
    )]
    Newer { path: String, found: i64, supported: i64 },
    #[error(
        "the database at {path} is at schema version {found}, older than the oldest this build upgrades ({oldest}); move it aside to start a new one"
    )]
    Incompatible { path: String, found: i64, oldest: i64 },
    #[error("cannot move the database aside at {path}: {detail}")]
    Archive { path: String, detail: String },
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
    /// The root's log was in a database this build could not upgrade, which was archived.
    /// Opening it again would restart it at the starting label.
    #[error("root {root} was recorded in an archived database")]
    Archived { root: String },
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
    #[error("the embedded read no longer owns its claim")]
    ReadClaimLost,
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[cfg(feature = "postgres")]
    #[error("PostgreSQL storage failure: {0}")]
    Postgres(#[from] postgres::PostgresError),
    #[cfg(feature = "fault-injection")]
    #[error("injected failure before commit")]
    Injected,
}

/// One read-decide-append step on one root's log; see [`LogStore::step`]. An append ends the
/// step; dropping it without one writes nothing. On PostgreSQL the step holds the family's row
/// lock from its read to its append; on SQLite it relies on the compare-and-swap alone.
pub struct Step<'a> {
    store: &'a LogStore,
    root: TrajectoryId,
    backend: StepBackend<'a>,
}

enum StepBackend<'a> {
    /// Borrows nothing; the lifetime is the PostgreSQL step's.
    Sqlite(std::marker::PhantomData<&'a ()>),
    #[cfg(feature = "postgres")]
    Postgres(postgres::PostgresStep<'a>),
}

impl Step<'_> {
    /// The root's log as it stands within this step.
    pub fn log(&self) -> Result<Log, ReadError> {
        match &self.backend {
            StepBackend::Sqlite(_) => self.store.log(&self.root),
            #[cfg(feature = "postgres")]
            StepBackend::Postgres(step) => {
                #[cfg(feature = "fault-injection")]
                self.store.read_refused()?;
                step.log()
            }
        }
    }

    /// [`LogStore::append`] within this step.
    pub fn append(self, based_on: &Log, facts: &[Fact]) -> Result<(), AppendError> {
        self.write(based_on, encode(facts, None), None, None)
    }

    /// [`LogStore::append_host`] within this step.
    pub fn append_host(self, based_on: &Log, facts: &[Fact], observation: &HostObservation) -> Result<(), AppendError> {
        let key = observation.key();
        self.write(based_on, encode(facts, Some(observation)), key.as_deref(), None)
    }

    /// [`LogStore::append_holding_embedded_read`] within this step.
    pub fn append_holding_embedded_read(
        self,
        based_on: &Log,
        facts: &[Fact],
        observation: Option<&HostObservation>,
        hold: &embedded::EmbeddedAppendHold<'_>,
    ) -> Result<(), AppendError> {
        let key = observation.and_then(HostObservation::key);
        self.write(based_on, encode(facts, observation), key.as_deref(), Some(hold))
    }

    fn write(
        self,
        based_on: &Log,
        bytes: Vec<u8>,
        key: Option<&str>,
        hold: Option<&embedded::EmbeddedAppendHold<'_>>,
    ) -> Result<(), AppendError> {
        assert_eq!(based_on.root, self.root, "a step appends only to the log it read");
        match self.backend {
            StepBackend::Sqlite(_) => match hold {
                None => self.store.append_at(&based_on.root, based_on.basis, bytes, key),
                Some(hold) => match &self.store.store {
                    Store::Sqlite(sqlite) => {
                        sqlite
                            .appender()
                            .append_holding_read(&based_on.root, based_on.basis, &bytes, key, hold)
                    }
                    #[cfg(feature = "postgres")]
                    Store::Postgres(_) => unreachable!("a SQLite step belongs to a SQLite store"),
                },
            },
            #[cfg(feature = "postgres")]
            StepBackend::Postgres(step) => step.append(based_on.basis, bytes, key, hold),
        }
    }
}

impl LogStore {
    /// A PostgreSQL view whose retained content uses this request's cipher.
    /// It owns a separate projection cache, so a view without the key cannot
    /// read a previously decrypted projection from another request.
    #[cfg(feature = "postgres")]
    pub fn encrypted_view(&self, cipher: encryption::StorageCipher) -> Result<LogStore, postgres::LeaseError> {
        match &self.store {
            Store::Sqlite(_) => Err(postgres::LeaseError::NotPostgres),
            Store::Postgres(pg) => Ok(LogStore {
                store: Store::Postgres(pg.encrypted_view(cipher)),
                #[cfg(feature = "fault-injection")]
                faults: std::sync::Arc::clone(&self.faults),
            }),
        }
    }

    /// The cipher of this view, for content an embedded host retains beside
    /// the event log. Its key is never returned or serialized.
    #[cfg(feature = "postgres")]
    pub fn cipher(&self) -> &encryption::StorageCipher {
        match &self.store {
            Store::Sqlite(_) => &encryption::StorageCipher::Plain,
            Store::Postgres(pg) => &pg.cipher,
        }
    }

    /// The connection a leased PostgreSQL store runs on, for host SQL beside its event batches
    /// and receipts. `None` for a SQLite store and for a store that is not leased.
    #[cfg(feature = "postgres")]
    pub fn postgres(&self) -> Option<&postgres::LeasedPostgres> {
        match &self.store {
            Store::Sqlite(_) => None,
            Store::Postgres(pg) => pg.leased(),
        }
    }

    /// This store over one pooled connection of its own. Everything the leased store does —
    /// event batches, receipts, host SQL — runs on that connection, and the connection goes
    /// back to the pool when the leased store has dropped.
    #[cfg(feature = "postgres")]
    pub fn lease(&self) -> Result<LogStore, postgres::LeaseError> {
        match &self.store {
            Store::Sqlite(_) => Err(postgres::LeaseError::NotPostgres),
            Store::Postgres(pg) => Ok(LogStore {
                store: Store::Postgres(pg.lease()?),
                #[cfg(feature = "fault-injection")]
                faults: std::sync::Arc::clone(&self.faults),
            }),
        }
    }

    /// Open the SQLite file at `path`, upgrading an older schema in place. A database too old
    /// to upgrade is moved aside to the returned path and replaced by a fresh one. Its roots
    /// stay closed: [`LogStore::create_root`] refuses each with [`CreateError::Archived`].
    pub fn open_archiving(path: &Path) -> Result<(LogStore, Option<PathBuf>), OpenError> {
        let (sqlite, archive) = Sqlite::open_archiving(path)?;
        Ok((
            LogStore {
                store: Store::Sqlite(sqlite),
                #[cfg(feature = "fault-injection")]
                faults: Default::default(),
            },
            archive,
        ))
    }

    /// Open the log. A fresh database gets the schema and its version stamp; an existing one is
    /// checked for damage, upgraded in place from a version this build knows how to upgrade,
    /// and refused otherwise.
    pub fn open(backend: Backend) -> Result<LogStore, OpenError> {
        let store = match backend {
            Backend::Sqlite { path } => Store::Sqlite(Sqlite::open(&path)?),
            Backend::Memory => Store::Sqlite(Sqlite::memory()?),
            #[cfg(feature = "postgres")]
            Backend::Postgres { url, max_connections } => {
                Store::Postgres(postgres::PostgresStore::open(url, max_connections)?)
            }
        };
        Ok(LogStore {
            store,
            #[cfg(feature = "fault-injection")]
            faults: Default::default(),
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
        match &self.store {
            Store::Sqlite(sqlite) => sqlite
                .create(&root, &key, policy_file, &bytes, || {
                    // A refusal here rolls the transaction back, exactly as a process kill before
                    // the commit would leave the file.
                    #[cfg(feature = "fault-injection")]
                    if self.failure_fires() {
                        return Err(CreateError::Injected);
                    }
                    Ok(())
                })
                .map(|()| root),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.create(&root, &key, policy_file, bytes),
        }
    }

    /// Whether this root has a log at all. The cheap question a caller asks before it decides
    /// to open one — reading the whole log to learn only this would cost the caller a second
    /// read on the path that then goes on to read it properly.
    pub fn has_root(&self, root: &TrajectoryId) -> Result<bool, ReadError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.has_root(root),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.has_root(root).map_err(Into::into),
        }
    }

    /// Read one root's whole log, with the position it stands at and the policy file it opened
    /// under.
    pub fn log(&self, root: &TrajectoryId) -> Result<Log, ReadError> {
        #[cfg(feature = "fault-injection")]
        self.read_refused()?;
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.log(root),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.log(root),
        }
    }

    /// Open a canonical workspace's append-only file history. Concurrent creators may race;
    /// the winning opening remains authoritative and callers validate its policy and Label.
    pub(crate) fn open_workspace(
        &self,
        workspace: &str,
        policy: &str,
        initial: &appa_engine::label::Label,
    ) -> Result<(), files::FileStoreError> {
        let event = files::WorkspaceEvent::Opened {
            policy: policy.to_owned(),
            initial: initial.clone(),
        };
        let payload = serde_json::to_vec(&event)?;
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.create_workspace(workspace, &payload),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.create_workspace(workspace, payload),
        }
    }

    /// Read and strictly decode one canonical workspace's complete event history.
    pub(crate) fn workspace_log(&self, workspace: &str) -> Result<files::WorkspaceLog, files::FileStoreError> {
        let batches = match &self.store {
            Store::Sqlite(sqlite) => sqlite.workspace_batches(workspace)?,
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.workspace_batches(workspace)?,
        };
        if batches.is_empty() {
            return Err(files::FileStoreError::Corrupt(
                "the workspace event log is missing".into(),
            ));
        }
        let events = batches
            .iter()
            .map(|batch| serde_json::from_slice(batch))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(files::WorkspaceLog {
            workspace: workspace.to_owned(),
            basis: events.len() as u64,
            events,
        })
    }

    /// Append one transition if the workspace history still stands at the read position.
    pub(crate) fn append_workspace(
        &self,
        based_on: &files::WorkspaceLog,
        event: &files::WorkspaceEvent,
    ) -> Result<(), files::FileStoreError> {
        let payload = serde_json::to_vec(event)?;
        let root = match event {
            files::WorkspaceEvent::RootBound { root } => Some(root),
            _ => None,
        };
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.append_workspace(&based_on.workspace, based_on.basis, &payload, root),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.append_workspace(&based_on.workspace, based_on.basis, payload, root.cloned()),
        }
    }

    /// Bind a root to a workspace by appending the binding event and its rebuildable index row
    /// in one transaction. An identical existing binding is idempotent.
    pub(crate) fn bind_workspace_root(
        &self,
        workspace: &str,
        root: &TrajectoryId,
    ) -> Result<(), files::FileStoreError> {
        loop {
            let log = self.workspace_log(workspace)?;
            let event_exists = log
                .events
                .iter()
                .any(|event| matches!(event, files::WorkspaceEvent::RootBound { root: bound } if bound == root));
            if let Some(bound) = self.workspace_for_root(root)? {
                if bound != workspace {
                    return Err(files::FileStoreError::Configuration(
                        "a root cannot change its tracked workspace".into(),
                    ));
                }
                return if event_exists {
                    Ok(())
                } else {
                    Err(files::FileStoreError::Corrupt(
                        "the derived root index has no authoritative binding event".into(),
                    ))
                };
            }
            if event_exists {
                return Err(files::FileStoreError::Corrupt(
                    "an authoritative root binding is missing from its derived index".into(),
                ));
            }
            match self.append_workspace(&log, &files::WorkspaceEvent::RootBound { root: root.clone() }) {
                Ok(()) => return Ok(()),
                Err(files::FileStoreError::Conflict { .. }) => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Resolve the derived root-binding index. The binding event remains the authority.
    pub(crate) fn workspace_for_root(&self, root: &TrajectoryId) -> Result<Option<String>, files::FileStoreError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.workspace_for_root(root),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.workspace_for_root(root),
        }
    }

    /// Append records to the log `based_on` was read from, only if it still stands where that
    /// read left it. A conflict writes nothing; the caller reads again and replays.
    pub fn append(&self, based_on: &Log, facts: &[Fact]) -> Result<(), AppendError> {
        self.append_at(&based_on.root, based_on.basis, encode(facts, None), None)
    }

    /// Append one host observation, and the engine facts it belongs with. The observation is
    /// durable exactly when those facts are, and a stale read writes nothing, just as append.
    pub fn append_host(
        &self,
        based_on: &Log,
        facts: &[Fact],
        observation: &HostObservation,
    ) -> Result<(), AppendError> {
        self.append_at(
            &based_on.root,
            based_on.basis,
            encode(facts, Some(observation)),
            observation.key().as_deref(),
        )
    }

    /// Every root whose host records ever named `key`, and nothing of what they recorded.
    ///
    /// The store answers "which families may stand behind this" without the caller naming
    /// them and without decoding a single row: the caller reads the families it gets back.
    /// The answer stays small by what a key is for — at most one root for an offer, and one
    /// root per session that quoted an identical ticket. The lookup is one index probe on the
    /// host keys table, whatever the log holds, so a key that stands for nothing costs the
    /// same as one that does.
    pub fn roots_mentioning(&self, key: &str) -> Result<Vec<TrajectoryId>, ReadError> {
        #[cfg(feature = "fault-injection")]
        self.read_refused()?;
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.roots_mentioning(key),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.roots_mentioning(key),
        }
    }

    /// One batch at one position, and the key row beside it where the batch names a key.
    fn append_at(&self, root: &TrajectoryId, basis: u64, bytes: Vec<u8>, key: Option<&str>) -> Result<(), AppendError> {
        match &self.store {
            Store::Sqlite(sqlite) => {
                #[allow(unused_mut, reason = "only the fault-injection build races the append")]
                let mut appender = sqlite.appender();
                #[cfg(feature = "fault-injection")]
                if self.contention_fires() {
                    self.contend(&mut appender, root)?;
                }
                appender.append(root, basis, &bytes, key, || {
                    #[cfg(feature = "fault-injection")]
                    if self.failure_fires() {
                        return Err(AppendError::Injected);
                    }
                    Ok(())
                })
            }
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.append(root, basis, bytes, key),
        }
    }

    /// Claims an operation receipt under `lease` before starting work. Completed receipts return
    /// saved decisions; a pending one is the caller's again, taken over once its lease has run
    /// out, or held by its live owner.
    pub fn claim_operation(
        &self,
        request: OperationRequest,
        lease: &ReceiptLease,
    ) -> Result<OperationClaim, ReceiptError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.claim_operation(&request, lease),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.claim_operation(request, lease.clone()),
        }
    }

    /// Completes a claimed operation receipt with its final decision, only while `owner` still
    /// holds its lease.
    pub fn complete_operation(
        &self,
        key: OperationKey,
        decision: serde_json::Value,
        owner: &LeaseOwner,
    ) -> Result<(), ReceiptError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.complete_operation(&key, &decision, owner),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.complete_operation(key, decision, owner.clone()),
        }
    }

    /// Claims a durable processed-result receipt under `lease` before result processing, as
    /// [`LogStore::claim_operation`] claims an operation.
    pub fn claim_processed_result(
        &self,
        request: ProcessedResultRequest,
        lease: &ReceiptLease,
    ) -> Result<ProcessedResultClaim, ReceiptError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.claim_processed_result(&request, lease),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.claim_processed_result(request, lease.clone()),
        }
    }

    /// Completes a processed-result receipt with its approved output and decision, only while
    /// `owner` still holds its lease.
    pub fn complete_processed_result(
        &self,
        key: ProcessedResultKey,
        approved_output: String,
        decision: serde_json::Value,
        owner: &LeaseOwner,
    ) -> Result<(), ReceiptError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.complete_processed_result(&key, &approved_output, &decision, owner),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.complete_processed_result(key, approved_output, decision, owner.clone()),
        }
    }

    /// Extends the lease on each listed receipt still pending under the lease's owner, in one
    /// transaction. [`Renewal::Lost`] when any listed receipt is gone or another owner took it
    /// over, pending or complete; a receipt the caller completed is kept.
    pub fn renew_receipts(
        &self,
        lease: &ReceiptLease,
        operations: &[OperationKey],
        results: &[ProcessedResultKey],
    ) -> Result<Renewal, ReceiptError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.renew_receipts(lease, operations, results),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.renew_receipts(lease.clone(), operations.to_vec(), results.to_vec()),
        }
    }

    /// Ends `owner`'s lease on each listed receipt still pending under it, so the next claim by
    /// another owner recovers it at once and drives its work again. The rows stay, with the
    /// input they recorded; completed receipts and other owners' receipts are untouched.
    pub fn release_receipts(
        &self,
        owner: &LeaseOwner,
        operations: &[OperationKey],
        results: &[ProcessedResultKey],
    ) -> Result<(), ReceiptError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.release_receipts(owner, operations, results),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.release_receipts(owner.clone(), operations.to_vec(), results.to_vec()),
        }
    }

    /// Checks whether pending receipts exist for a root trajectory.
    pub fn has_pending_receipts(&self, root: &TrajectoryId) -> Result<bool, ReceiptError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.has_pending_receipts(root),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.has_pending_receipts(root).map_err(Into::into),
        }
    }

    /// Hold a peer message for `receiver`, not yet notified, until `now + ttl`. The same
    /// transaction drops the receiver's expired messages, then its oldest beyond `quota`.
    #[allow(clippy::too_many_arguments, reason = "each argument is one column of the held row")]
    pub fn hold_peer_message(
        &self,
        receiver: &TrajectoryId,
        digest: PeerDigest,
        label: &Label,
        body: &str,
        now: SystemTime,
        ttl: Duration,
        quota: usize,
    ) -> Result<HeldNotice, HeldError> {
        let held = held::NewHeld::new(label, now, ttl)?;
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.hold_peer_message(receiver, digest, body, &held, now, quota),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.hold_peer_message(receiver, digest, body, &held, now, quota),
        }?;
        Ok(held.notice(digest, label))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn claim_embedded_peer(
        &self,
        root: &TrajectoryId,
        sender: &TrajectoryId,
        recipient: &TrajectoryId,
        pending_spawn: Option<&str>,
        dispatch: &str,
        digest: &PeerDigest,
        label: &Label,
        body: &str,
        now: SystemTime,
    ) -> Result<EmbeddedClaim, EmbeddedError> {
        let fresh = embedded::NewEmbedded::fresh(
            root.as_str(),
            sender.as_str(),
            recipient.as_str(),
            pending_spawn,
            dispatch,
            digest,
            label,
            body,
            now,
        )?;
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.claim_embedded_peer(&fresh),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.claim_embedded_peer(&fresh),
        }
    }

    pub fn load_embedded_peer(&self, root: &TrajectoryId, id: &str) -> Result<Option<EmbeddedRow>, EmbeddedError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.load_embedded_peer(root.as_str(), id),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.load_embedded_peer(root.as_str(), id),
        }
    }

    /// Null bodies of held and direct rows that can no longer be returned. Proof rows stay.
    /// A completed read receipt is not unread inbox data and is left alone.
    pub fn expire_embedded_inbox(&self, root: &TrajectoryId, now: SystemTime) -> Result<(), EmbeddedError> {
        let now = embedded::millis(now);
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.expire_embedded_inbox(root.as_str(), now),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.expire_embedded_inbox(root.as_str(), now),
        }
    }

    pub fn list_embedded_peer(
        &self,
        root: &TrajectoryId,
        recipient: &TrajectoryId,
        now: SystemTime,
    ) -> Result<Vec<EmbeddedRow>, EmbeddedError> {
        let now = embedded::millis(now);
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.list_embedded_peer(root.as_str(), recipient.as_str(), now),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.list_embedded_peer(root.as_str(), recipient.as_str(), now),
        }
    }

    pub fn take_embedded_direct(
        &self,
        root: &TrajectoryId,
        id: &str,
        sender: &TrajectoryId,
        recipient: &TrajectoryId,
        digest: &PeerDigest,
    ) -> Result<DirectTake, EmbeddedError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.take_embedded_direct(
                root.as_str(),
                id,
                sender.as_str(),
                recipient.as_str(),
                &digest.to_string(),
            ),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.take_embedded_direct(
                root.as_str(),
                id,
                sender.as_str(),
                recipient.as_str(),
                &digest.to_string(),
            ),
        }
    }

    pub fn claim_embedded_read(
        &self,
        root: &TrajectoryId,
        id: &str,
        recipient: &TrajectoryId,
        call_id: &str,
        arguments: &str,
    ) -> Result<ReadTake, EmbeddedError> {
        match &self.store {
            Store::Sqlite(sqlite) => {
                sqlite.claim_embedded_read(root.as_str(), id, recipient.as_str(), call_id, arguments)
            }
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.claim_embedded_read(root.as_str(), id, recipient.as_str(), call_id, arguments),
        }
    }

    /// Store the receipt only if `generation` still owns the read. `false` means another
    /// caller took the ticket or the row was released; nothing was written.
    pub fn finish_embedded_read(
        &self,
        root: &TrajectoryId,
        id: &str,
        call_id: &str,
        generation: i64,
        decision: &str,
    ) -> Result<bool, EmbeddedError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.finish_embedded_read(root.as_str(), id, call_id, generation, decision),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.finish_embedded_read(root.as_str(), id, call_id, generation, decision),
        }
    }

    /// Return an unopened read to the inbox only if `generation` still holds the opening
    /// ticket. A bound or finished read is left alone. `false` means this caller no longer owns it.
    pub fn release_embedded_read(
        &self,
        root: &TrajectoryId,
        id: &str,
        call_id: &str,
        generation: i64,
    ) -> Result<bool, EmbeddedError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.release_embedded_read(root.as_str(), id, call_id, generation),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.release_embedded_read(root.as_str(), id, call_id, generation),
        }
    }

    /// Append engine facts only while this read owns its ticket. A batch that opens a dispatch
    /// or admits a value binds the ticket in the same transaction, so a release cannot put the
    /// row back in the inbox after the log has consumed the message.
    pub fn append_holding_embedded_read(
        &self,
        based_on: &Log,
        facts: &[Fact],
        observation: Option<&HostObservation>,
        hold: &embedded::EmbeddedAppendHold<'_>,
    ) -> Result<(), AppendError> {
        let bytes = encode(facts, observation);
        let key = observation.and_then(HostObservation::key);
        match &self.store {
            Store::Sqlite(sqlite) => {
                sqlite
                    .appender()
                    .append_holding_read(&based_on.root, based_on.basis, &bytes, key.as_deref(), hold)
            }
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.append_holding_read(&based_on.root, based_on.basis, bytes, key.as_deref(), hold),
        }
    }

    /// Begin one read-decide-append step on `root`'s log. On PostgreSQL the step is one
    /// transaction that holds the root's row lock from its read to its append, so the writers
    /// of one family take turns rather than losing the compare-and-swap to each other; the lock
    /// and the connection last for this step only. On a leased store the step runs on the
    /// lease's connection, so the host keeps no transaction of its own open there until the
    /// step ends. On SQLite the step adds nothing to the compare-and-swap. Every read and
    /// append of the step goes through the [`Step`].
    pub fn step(&self, root: &TrajectoryId) -> Result<Step<'_>, ReadError> {
        let backend = match &self.store {
            Store::Sqlite(_) => StepBackend::Sqlite(std::marker::PhantomData),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => StepBackend::Postgres(pg.step(root)?),
        };
        Ok(Step {
            store: self,
            root: root.clone(),
            backend,
        })
    }

    /// The receiver's unexpired messages it was not yet told of, oldest first. Each is told
    /// once: the same transaction marks them notified.
    pub fn peer_notices(&self, receiver: &TrajectoryId, now: SystemTime) -> Result<Vec<HeldNotice>, HeldError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.peer_notices(receiver, now),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.peer_notices(receiver, now),
        }
    }

    /// One unexpired held message of the receiver, without its body and without taking it.
    pub fn peek_peer_message(
        &self,
        receiver: &TrajectoryId,
        id: &HeldPeerId,
        now: SystemTime,
    ) -> Result<Option<HeldNotice>, HeldError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.peek_peer_message(receiver, id, now),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.peek_peer_message(receiver, id, now),
        }
    }

    /// Take one held message of the receiver with its body, deleting it. An expired message is
    /// deleted and reads as `None`.
    pub fn take_peer_message(
        &self,
        receiver: &TrajectoryId,
        id: &HeldPeerId,
        now: SystemTime,
    ) -> Result<Option<HeldPeerMessage>, HeldError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.take_peer_message(receiver, id, now),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.take_peer_message(receiver, id, now),
        }
    }
}

#[cfg(feature = "fault-injection")]
impl LogStore {
    /// Drop a stored read receipt without touching the engine log. Tests use this to
    /// simulate a crash after admission and before the receipt was written.
    pub fn testing_clear_embedded_decision(&self, root: &TrajectoryId, id: &str) -> Result<(), EmbeddedError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.testing_clear_embedded_decision(root.as_str(), id),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.testing_clear_embedded_decision(root.as_str(), id),
        }
    }

    /// Force a row past its TTL without waiting. The next list, send, or receive nulls a
    /// held or direct body. A completed read receipt is left.
    pub fn testing_expire_embedded(&self, root: &TrajectoryId, id: &str) -> Result<(), EmbeddedError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.testing_expire_embedded(root.as_str(), id),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.testing_expire_embedded(root.as_str(), id),
        }
    }

    /// Replace the stored body. Tests use this to show recovery refuses an admission that
    /// no longer matches the row.
    pub fn testing_replace_embedded_body(
        &self,
        root: &TrajectoryId,
        id: &str,
        body: &str,
    ) -> Result<(), EmbeddedError> {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite.testing_replace_embedded_body(root.as_str(), id, body),
            #[cfg(feature = "postgres")]
            Store::Postgres(pg) => pg.testing_replace_embedded_body(root.as_str(), id, body),
        }
    }

    /// A foreign writer wins the race in its own committed transaction, exactly as a second
    /// process would. It takes the position and records nothing, so this caller's append
    /// conflicts on position and replays, and an assertion reads whose write landed from the
    /// position rather than from records a later read would have to accept.
    ///
    /// Where the injection names an observation, the winner records that instead, and where it
    /// names another family it records there and still takes this one's position: a foreign
    /// writer that changed a sibling's log is the race a reader of several families has to
    /// survive.
    fn contend(&self, appender: &mut sqlite::Appender<'_>, root: &TrajectoryId) -> Result<(), rusqlite::Error> {
        let armed = self
            .faults
            .contending_record
            .lock()
            .expect("the injection mutex is never poisoned")
            .take()
            .filter(|(racing, _, _)| racing == root);
        match &armed {
            Some((_, recorded_in, observation)) if recorded_in != root => {
                appender.foreign(&[(recorded_in, Some(observation)), (root, None)])
            }
            Some((_, recorded_in, observation)) => appender.foreign(&[(recorded_in, Some(observation))]),
            None => appender.foreign(&[(root, None)]),
        }
    }

    /// Arm the fail point: `skip` commits land normally and the one after them rolls back, as a
    /// process kill inside the transaction would. A PostgreSQL store never consults it.
    pub fn fail_commit_after(&self, skip: u64) {
        self.faults
            .commits_until_failure
            .store(skip + 1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Arm the read fail point: the next `count` reads answer with a failure instead of the
    /// store's rows. A caller that refuses without asking the store leaves the arming where
    /// it was, so the read that comes after it still meets the failure.
    pub fn fail_next_reads(&self, count: u64) {
        self.faults
            .failing_reads
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    /// Arm the contention point: the next `count` appends are raced by a foreign writer that
    /// wins, so each loses the compare-and-swap and its caller replays. A PostgreSQL store
    /// never consults it.
    pub fn contend_next_appends(&self, count: u64) {
        self.faults
            .contended_appends
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    /// Arm the contention point once, with what the winner records. The next append to `root`
    /// loses to a writer that put `observation` in the log, so the caller's next derivation
    /// answers to a state another writer changed rather than to a position it only moved.
    pub fn contend_next_append_with(
        &self,
        racing: &TrajectoryId,
        recorded_in: &TrajectoryId,
        observation: &HostObservation,
    ) {
        *self
            .faults
            .contending_record
            .lock()
            .expect("the injection mutex is never poisoned") =
            Some((racing.clone(), recorded_in.clone(), observation.clone()));
        self.contend_next_appends(1);
    }

    /// Forget every stored policy file, leaving each root's opening naming a
    /// file this database no longer holds. Damage stated in this
    /// crate's own vocabulary, so a caller can pin how it refuses without
    /// learning the schema. SQLite only.
    pub fn forget_policy_files(&self) {
        self.sqlite_only().forget_policy_files();
    }

    /// Replace the bytes of every stored policy file, so each stops hashing to
    /// the key its roots' openings name. SQLite only.
    pub fn corrupt_policy_files(&self, bytes: &[u8]) {
        self.sqlite_only().corrupt_policy_files(bytes);
    }

    /// Replace what one batch of a root's log holds. The bytes are stored as
    /// given, so a caller can leave records that do not decode, or records
    /// that decode but are not the history they claim to be. SQLite only.
    pub fn corrupt_batch(&self, root: &TrajectoryId, seq: u64, bytes: &[u8]) {
        let changed = self.sqlite_only().corrupt_batch(root, seq, bytes);
        assert_eq!(changed, 1, "the batch to corrupt exists");
    }

    fn sqlite_only(&self) -> &Sqlite {
        match &self.store {
            Store::Sqlite(sqlite) => sqlite,
            #[cfg(feature = "postgres")]
            Store::Postgres(_) => panic!("SQLite-only operation"),
        }
    }

    fn failure_fires(&self) -> bool {
        consume(&self.faults.commits_until_failure) == Some(1)
    }

    fn contention_fires(&self) -> bool {
        consume(&self.faults.contended_appends).is_some()
    }

    fn read_refused(&self) -> Result<(), ReadError> {
        match consume(&self.faults.failing_reads) {
            Some(_) => Err(ReadError::Injected),
            None => Ok(()),
        }
    }
}

#[cfg(feature = "fault-injection")]
fn consume(counter: &std::sync::atomic::AtomicU64) -> Option<u64> {
    use std::sync::atomic::Ordering::SeqCst;
    counter
        .try_update(SeqCst, SeqCst, |remaining| match remaining {
            0 => None,
            remaining => Some(remaining - 1),
        })
        .ok()
}

fn opened_by(opening: &[Fact]) -> Result<(TrajectoryId, PolicyFileKey), CreateError> {
    match opening.first() {
        Some(Fact::TrajectoryOpened(appa_engine::fact::TrajectoryOpening {
            trajectory,
            policy_file_key,
            ..
        })) => Ok((trajectory.clone(), policy_file_key.clone())),
        Some(_) => Err(CreateError::Malformed {
            detail: "the first record is not a TrajectoryOpened".to_string(),
        }),
        None => Err(CreateError::Malformed {
            detail: "the batch is empty".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    /// The SQLite connection, for tests that reach under the log's API.
    impl LogStore {
        pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
            match &self.store {
                Store::Sqlite(sqlite) => sqlite.connection(),
                #[cfg(feature = "postgres")]
                Store::Postgres(_) => panic!("SQLite-only operation"),
            }
        }
    }

    const POLICY: &str = r#"
        version = 2
    "#;

    fn engine() -> appa_engine::engine::Engine {
        appa_policy::Config::from_toml_str(POLICY)
            .expect("the minimal policy compiles")
            .engine()
            .clone()
    }

    pub(crate) fn root() -> TrajectoryId {
        TrajectoryId::new("cc:root")
    }

    fn opening(id: &TrajectoryId) -> Vec<Fact> {
        engine()
            .open_trajectory(id, PolicyFileKey::of(POLICY.as_bytes()), None)
            .expect("the opening seals")
            .into_unsealed()
    }

    pub(crate) fn punctuation() -> Vec<Fact> {
        vec![Fact::Boundary {
            trajectory: root(),
            kind: appa_engine::fact::BoundaryKind::VoidReturn,
        }]
    }

    /// A database file holding `root()`, rewritten to look as a build at `version` left it:
    /// stamped `version`, without the tables and columns later versions added.
    fn written_at(version: i64) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        LogStore::open(Backend::Sqlite { path: path.clone() })
            .expect("a fresh store opens")
            .create_root(opening(&root()), POLICY.as_bytes())
            .expect("the root opens");
        let old = rusqlite::Connection::open(&path).expect("the file reopens");
        old.execute_batch(
            "DROP TABLE held_peer_messages; DROP TABLE archived_roots;
             ALTER TABLE operations DROP COLUMN lease_owner;
             ALTER TABLE operations DROP COLUMN lease_expires_at;
             ALTER TABLE processed_results DROP COLUMN lease_owner;
             ALTER TABLE processed_results DROP COLUMN lease_expires_at;
             ALTER TABLE processed_results DROP COLUMN completed_at;",
        )
        .expect("the later tables and columns drop");
        old.pragma_update(None, "user_version", version)
            .expect("the version moves back");
        (dir, path)
    }

    #[test]
    fn the_oldest_upgradable_database_upgrades_in_place_and_keeps_its_logs() {
        let (_dir, path) = written_at(5);
        let (store, archive) = LogStore::open_archiving(&path).expect("the database upgrades");
        assert_eq!(archive, None);
        assert_eq!(
            store.log(&root()).expect("the old log reads").policy_file(),
            POLICY.as_bytes()
        );
        drop(store);
        LogStore::open(Backend::Sqlite { path }).expect("the upgraded database is current");
    }

    /// A pending receipt a database carried from before receipts held leases has none once
    /// upgraded, so the next claim recovers it.
    #[test]
    fn an_upgraded_pending_receipt_is_recovered() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        drop(LogStore::open(Backend::Sqlite { path: path.clone() }).expect("a fresh store opens"));
        let old = rusqlite::Connection::open(&path).expect("the file reopens");
        old.execute_batch(
            "ALTER TABLE operations DROP COLUMN lease_owner;
             ALTER TABLE operations DROP COLUMN lease_expires_at;
             ALTER TABLE processed_results DROP COLUMN lease_owner;
             ALTER TABLE processed_results DROP COLUMN lease_expires_at;
             ALTER TABLE processed_results DROP COLUMN completed_at;",
        )
        .expect("the lease columns drop");
        old.execute(
            "INSERT INTO operations (organization_id, caller_id, session_id, operation_id, root, input, status)
             VALUES ('org', 'caller', 'session', 'op-1', 'root', ?1, 'pending')",
            [r#"{"version":1,"binding":"caller","semantic":{"tool":"wire"}}"#],
        )
        .expect("the pending receipt lands");
        old.pragma_update(None, "user_version", 7)
            .expect("the version moves back");
        drop(old);

        let store = LogStore::open(Backend::Sqlite { path }).expect("the database upgrades");
        let request = OperationRequest {
            key: OperationKey {
                session: SessionScope {
                    organization_id: "org".to_owned(),
                    session_id: "session".to_owned(),
                },
                binding: caller_binding(),
                operation_id: "op-1".to_owned(),
            },
            root: TrajectoryId::new("root"),
            input: serde_json::json!({"tool": "wire"}),
            context: None,
        };
        assert_eq!(
            store
                .claim_operation(request, &receipt_lease("a"))
                .expect("the claim resolves"),
            OperationClaim::Recovered
        );
    }

    #[test]
    fn a_database_too_old_to_upgrade_is_archived_and_its_roots_stay_closed() {
        let (dir, path) = written_at(4);
        assert!(matches!(
            LogStore::open(Backend::Sqlite { path: path.clone() }).err(),
            Some(OpenError::Incompatible { found: 4, .. })
        ));

        let (store, archive) = LogStore::open_archiving(&path).expect("the database is replaced");
        let archive = archive.expect("the old database is moved aside");
        assert!(matches!(
            store.create_root(opening(&root()), POLICY.as_bytes()),
            Err(CreateError::Archived { .. })
        ));
        let fresh = TrajectoryId::new("cc:fresh");
        store
            .create_root(opening(&fresh), POLICY.as_bytes())
            .expect("a new session opens");
        let kept: i64 = rusqlite::Connection::open(&archive)
            .expect("the archive opens")
            .query_row(
                "SELECT COUNT(*) FROM logs WHERE root = ?1",
                params![root().as_str()],
                |row| row.get(0),
            )
            .expect("the archived log reads");
        assert_eq!(kept, 1, "the archive keeps the old log");
        drop(store);

        let (store, again) = LogStore::open_archiving(&path).expect("the replacement reopens");
        assert_eq!(again, None);
        assert!(matches!(
            store.create_root(opening(&root()), POLICY.as_bytes()),
            Err(CreateError::Archived { .. })
        ));
        assert!(store.has_root(&fresh).expect("the check runs"));
        let archives = std::fs::read_dir(dir.path())
            .expect("the directory lists")
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().contains(".archived-"))
            })
            .count();
        assert_eq!(archives, 1);
    }

    #[test]
    fn concurrent_archivers_leave_one_archive_and_keep_its_roots_closed() {
        let (dir, path) = written_at(4);
        let opened: Vec<_> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| LogStore::open_archiving(&path).expect("the database opens")))
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().expect("the worker finishes"))
                .collect()
        });
        assert_eq!(opened.iter().filter(|(_, archive)| archive.is_some()).count(), 1);
        for (store, _) in &opened {
            assert!(matches!(
                store.create_root(opening(&root()), POLICY.as_bytes()),
                Err(CreateError::Archived { .. })
            ));
        }
        let archives = std::fs::read_dir(dir.path())
            .expect("the directory lists")
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().contains(".archived-"))
            })
            .count();
        assert_eq!(archives, 1);
    }

    #[test]
    fn a_database_another_connection_holds_is_not_archived() {
        let (_dir, path) = written_at(4);
        let holder = rusqlite::Connection::open(&path).expect("another connection opens");
        let _: i64 = holder
            .query_row("SELECT COUNT(*) FROM logs", [], |row| row.get(0))
            .expect("the other connection reads, then idles");

        assert!(matches!(
            LogStore::open_archiving(&path).err(),
            Some(OpenError::Incompatible { found: 4, .. })
        ));
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
        assert!(matches!(log.facts(), [Fact::TrajectoryOpened(_)]));
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
            [Fact::TrajectoryOpened(_), Fact::Boundary { .. }, Fact::Boundary { .. },],
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
    fn workspace_events_share_the_log_compare_and_swap_contract() {
        let dir = tempfile::tempdir().expect("a database directory exists");
        let path = dir.path().join("events.db");
        let first = LogStore::open(Backend::Sqlite { path: path.clone() }).expect("the first store opens");
        let second = LogStore::open(Backend::Sqlite { path }).expect("the second store opens");
        first
            .open_workspace("/workspace", "policy", &appa_engine::label::Label::top())
            .expect("the workspace opens");
        let first_read = first.workspace_log("/workspace").expect("the first read succeeds");
        let stale = second.workspace_log("/workspace").expect("the second read succeeds");
        first
            .append_workspace(
                &first_read,
                &files::WorkspaceEvent::RootBound {
                    root: TrajectoryId::new("first"),
                },
            )
            .expect("the first event lands");
        assert!(matches!(
            second.append_workspace(
                &stale,
                &files::WorkspaceEvent::RootBound {
                    root: TrajectoryId::new("second"),
                },
            ),
            Err(files::FileStoreError::Conflict { actual: 2, .. })
        ));
        assert_eq!(second.workspace_log("/workspace").unwrap().basis, 2);
    }

    pub(crate) fn observed(actor: &str, server: &str) -> HostObservation {
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
            prompt: None,
        };
        store.append_host(&log, &punctuation(), &bound).unwrap();

        let seen = store.log(&root()).unwrap();
        assert_eq!(seen.basis(), 2, "both streams took one position");
        assert!(matches!(
            seen.facts(),
            [Fact::TrajectoryOpened(_), Fact::Boundary { .. }]
        ));
        let bindings: Vec<_> = seen.call_bindings().collect();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].call_id, "toolu_1");
        assert_eq!(bindings[0].dispatch, &dispatch);
        assert_eq!(bindings[0].trajectory, &root());
    }

    /// The wire names a caller reports a failure class under.
    #[test]
    fn store_error_classes_serialize_to_their_frozen_wire_names() {
        let wire = |class: StoreErrorClass| match class {
            StoreErrorClass::UnknownRoot => "unknown_root",
            StoreErrorClass::AlreadyExists => "already_exists",
            StoreErrorClass::Archived => "archived",
            StoreErrorClass::PolicyUnavailable => "policy_unavailable",
            StoreErrorClass::PolicyMismatch => "policy_mismatch",
            StoreErrorClass::Undecodable => "undecodable",
            StoreErrorClass::Malformed => "malformed",
            StoreErrorClass::Conflict => "conflict",
            StoreErrorClass::Storage => "storage",
        };
        for class in [
            StoreErrorClass::UnknownRoot,
            StoreErrorClass::AlreadyExists,
            StoreErrorClass::Archived,
            StoreErrorClass::PolicyUnavailable,
            StoreErrorClass::PolicyMismatch,
            StoreErrorClass::Undecodable,
            StoreErrorClass::Malformed,
            StoreErrorClass::Conflict,
            StoreErrorClass::Storage,
        ] {
            assert_eq!(
                serde_json::to_value(class).expect("a class serializes"),
                serde_json::Value::String(wire(class).to_owned())
            );
        }
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
    fn a_log_with_a_sequence_gap_refuses_the_read() {
        let store = opened();
        for _ in 0..2 {
            let log = store.log(&root()).expect("the log reads");
            store.append(&log, &punctuation()).expect("the append lands");
        }
        store
            .lock()
            .execute("DELETE FROM logs WHERE seq = 1", [])
            .expect("the middle row deletes");
        let error = store.log(&root()).expect_err("a gapped log does not read");
        assert_eq!(StoreErrorClass::from(&error), StoreErrorClass::Storage, "{error:?}");
    }

    /// The position is one past the highest stored `seq`, not the row count, so the two
    /// differ only on a damaged log.
    #[test]
    fn the_append_position_follows_the_highest_stored_seq() {
        let store = opened();
        for _ in 0..2 {
            let log = store.log(&root()).expect("the log reads");
            store.append(&log, &punctuation()).expect("the append lands");
        }
        let log = store.log(&root()).expect("the log reads");
        store
            .lock()
            .execute("DELETE FROM logs WHERE seq = 1", [])
            .expect("the middle row deletes");
        store.append(&log, &punctuation()).expect("the append lands at seq 3");
        let seqs = store
            .lock()
            .prepare("SELECT seq FROM logs ORDER BY seq")
            .expect("the query prepares")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("the seqs read")
            .collect::<Result<Vec<_>, _>>()
            .expect("every seq reads");
        assert_eq!(seqs, [0, 2, 3]);
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
    fn a_key_finds_the_roots_that_named_it_and_no_others() {
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
            store.roots_mentioning("offer:one").unwrap(),
            vec![root(), second.clone()],
            "one entry per root, however many of its rows name the key"
        );
        assert_eq!(
            store.roots_mentioning("offer:two").unwrap(),
            vec![second.clone()],
            "a key nothing else names answers with the one root that does"
        );
        assert_eq!(
            store.roots_mentioning("offer:on").unwrap(),
            Vec::new(),
            "a key matches whole, so one key is never a prefix of another"
        );
    }

    fn peer_observations(root: &TrajectoryId) -> [HostObservation; 4] {
        let digest = PeerDigest::of_body("hello");
        [
            HostObservation::Addressed {
                address: PeerAddress::parse("uds:/tmp/appa/peer.sock").expect("the address parses"),
            },
            HostObservation::Titled {
                title: SessionTitle::parse("peer-a").expect("the title parses"),
            },
            HostObservation::PeerSent {
                digest,
                label: Label::new(
                    appa_engine::label::Trust::new(1),
                    appa_engine::label::Audience::public(),
                ),
                dispatch: DispatchId::new(
                    root.clone(),
                    serde_json::from_value(serde_json::json!("00".repeat(32))).expect("a call digest parses"),
                    0,
                ),
            },
            HostObservation::PeerAdmitted { digest },
        ]
    }

    #[test]
    fn peer_observations_name_their_keys_and_the_roots_that_recorded_them() {
        let store = opened();
        let [addressed, titled, sent, admitted] = peer_observations(&root());
        let address = PeerAddress::parse("uds:/tmp/appa/peer.sock").expect("the address parses");
        let title = SessionTitle::parse("peer-a").expect("the title parses");
        let digest = PeerDigest::of_body("hello");
        let keys = [
            peer_address_key(&address),
            peer_title_key(&title),
            peer_sent_key(&digest),
        ];
        assert_eq!(
            [&addressed, &titled, &sent].map(HostObservation::key),
            keys.clone().map(Some)
        );
        assert_eq!(admitted.key(), None);
        assert_eq!(
            keys,
            [
                "peer-address:uds:/tmp/appa/peer.sock".to_owned(),
                "peer-title:peer-a".to_owned(),
                format!("peer-sent:{digest}"),
            ]
        );

        for observation in [&addressed, &titled, &sent, &admitted] {
            store
                .append_host(&store.log(&root()).unwrap(), &[], observation)
                .unwrap();
        }
        for key in &keys {
            assert_eq!(store.roots_mentioning(key).unwrap(), vec![root()], "{key}");
        }
        assert_eq!(
            store
                .roots_mentioning(&peer_sent_key(&PeerDigest::of_body("other")))
                .unwrap(),
            Vec::new()
        );
        assert_eq!(
            observations(&store.log(&root()).unwrap()),
            vec![addressed, titled, sent, admitted],
            "the records read back as written"
        );
    }

    #[test]
    fn peer_observations_round_trip_under_their_wire_spelling() {
        let digest = PeerDigest::of_body("hello");
        for observation in peer_observations(&root()) {
            let wire = serde_json::to_value(&observation).expect("an observation serializes");
            assert_eq!(
                serde_json::from_value::<HostObservation>(wire).expect("an observation deserializes"),
                observation
            );
        }
        assert_eq!(
            serde_json::to_value(HostObservation::PeerAdmitted { digest }).unwrap(),
            serde_json::json!({"kind": "peer_admitted", "digest": digest.to_string()})
        );
        assert!(
            serde_json::from_value::<HostObservation>(serde_json::json!({
                "kind": "peer_admitted",
                "digest": digest.to_string(),
                "body": "hello",
            }))
            .is_err(),
            "an admitted record never carries the body"
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
    fn the_memory_backend_is_private_to_its_store() {
        let first = opened();
        assert!(first.log(&root()).is_ok());
        assert!(matches!(memory().log(&root()), Err(ReadError::UnknownRoot { .. })));
    }

    fn receipt_session(suffix: &str) -> SessionScope {
        SessionScope {
            organization_id: format!("receipt-org:{suffix}"),
            session_id: format!("receipt-session:{suffix}"),
        }
    }

    fn caller_binding() -> ReceiptBinding {
        ReceiptBinding::Caller {
            caller_id: "caller".to_owned(),
        }
    }

    pub(crate) fn lease_owner(name: &str) -> LeaseOwner {
        LeaseOwner::parse(name).expect("the owner is well formed")
    }

    pub(crate) fn receipt_lease(name: &str) -> ReceiptLease {
        ReceiptLease::new(lease_owner(name), Duration::from_secs(60)).expect("the lease is well formed")
    }

    fn typed_receipts_are_idempotent_and_fail_closed(store: &LogStore, suffix: &str) {
        let session = receipt_session(suffix);
        let root = TrajectoryId::new(format!("receipt-root:{suffix}"));
        let request = OperationRequest {
            key: OperationKey {
                session: session.clone(),
                binding: caller_binding(),
                operation_id: "remedy-1".to_owned(),
            },
            root: root.clone(),
            input: serde_json::json!({"offer_id": "0123456789abcdef"}),
            context: None,
        };
        let (lease, other) = (receipt_lease("a"), receipt_lease("b"));
        assert!(matches!(
            store.claim_operation(request.clone(), &lease),
            Ok(OperationClaim::Claimed)
        ));
        assert!(matches!(
            store.claim_operation(request.clone(), &other),
            Ok(OperationClaim::Held)
        ));
        let decision = serde_json::json!({"decision":"mcp_result"});
        store
            .complete_operation(request.key.clone(), decision.clone(), lease.owner())
            .expect("the claimed operation completes");
        assert_eq!(
            store
                .claim_operation(request.clone(), &other)
                .expect("the completed operation replays"),
            OperationClaim::Complete {
                decision: decision.clone()
            }
        );
        let mut changed = request.clone();
        changed.input = serde_json::json!({"offer_id": "other"});
        assert!(matches!(
            store.claim_operation(changed, &lease),
            Err(ReceiptError::InputMismatch)
        ));

        let result = ProcessedResultRequest {
            key: ProcessedResultKey {
                session: session.clone(),
                caller_id: Some("caller".to_owned()),
                tool_call_id: "call-1".to_owned(),
            },
            root: root.clone(),
        };
        assert!(matches!(
            store.claim_processed_result(result.clone(), &lease),
            Ok(ProcessedResultClaim::Claimed)
        ));
        assert!(store.has_pending_receipts(&root).expect("the pending receipt checks"));
        store
            .complete_processed_result(
                result.key.clone(),
                "approved".to_owned(),
                decision.clone(),
                lease.owner(),
            )
            .expect("the processed result completes");
        assert_eq!(
            store
                .claim_processed_result(result, &other)
                .expect("the processed result replays"),
            ProcessedResultClaim::Complete {
                approved_output: "approved".to_owned(),
                decision,
            }
        );
        assert!(!store.has_pending_receipts(&root).expect("all receipts are terminal"));
    }

    #[test]
    fn memory_typed_receipts_are_idempotent_and_fail_closed() {
        typed_receipts_are_idempotent_and_fail_closed(&memory(), "memory");
    }

    #[test]
    fn sqlite_typed_receipts_are_idempotent_and_fail_closed() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.db");
        let store = LogStore::open(Backend::Sqlite { path: path.clone() }).expect("a fresh store opens");
        typed_receipts_are_idempotent_and_fail_closed(&store, "sqlite");
        drop(store);
        let reopened = LogStore::open(Backend::Sqlite { path }).expect("the store reopens");
        assert!(matches!(
            reopened.claim_operation(
                OperationRequest {
                    key: OperationKey {
                        session: receipt_session("sqlite"),
                        binding: caller_binding(),
                        operation_id: "remedy-1".to_owned(),
                    },
                    root: TrajectoryId::new("receipt-root:sqlite"),
                    input: serde_json::json!({"offer_id": "0123456789abcdef"}),
                    context: None,
                },
                &receipt_lease("b")
            ),
            Ok(OperationClaim::Complete { .. })
        ));
    }

    #[test]
    fn memory_receipts_do_not_leak_across_stores() {
        let request = OperationRequest {
            key: OperationKey {
                session: receipt_session("private"),
                binding: caller_binding(),
                operation_id: "remedy-1".to_owned(),
            },
            root: TrajectoryId::new("receipt-root:private"),
            input: serde_json::json!({"offer_id": "0123456789abcdef"}),
            context: None,
        };
        assert_eq!(
            memory()
                .claim_operation(request.clone(), &receipt_lease("a"))
                .expect("the operation claims"),
            OperationClaim::Claimed
        );
        assert_eq!(
            memory()
                .claim_operation(request, &receipt_lease("b"))
                .expect("the other store claims"),
            OperationClaim::Claimed,
            "Memory receipts are private to the store that wrote them"
        );
    }

    /// Hosts derive session ids from client-supplied values that are unique only within an
    /// organization, so two organizations may present the same session, operation and tool
    /// call ids. Each must claim, complete and replay its own receipt.
    fn organizations_sharing_a_session_id_keep_their_receipts_apart(store: &LogStore, session_id: &str) {
        let scope = |organization: &str| SessionScope {
            organization_id: format!("{organization}:{session_id}"),
            session_id: session_id.to_owned(),
        };
        let (first, second) = (scope("org-a"), scope("org-b"));
        let operation = |scope: &SessionScope| OperationRequest {
            key: OperationKey {
                session: scope.clone(),
                binding: ReceiptBinding::Caller {
                    caller_id: "user:1".to_owned(),
                },
                operation_id: "op-1".to_owned(),
            },
            root: TrajectoryId::new(format!("receipt-root:{session_id}")),
            input: serde_json::json!({"tool": "wire"}),
            context: None,
        };
        let result = |scope: &SessionScope| ProcessedResultRequest {
            key: ProcessedResultKey {
                session: scope.clone(),
                caller_id: Some("user:1".to_owned()),
                tool_call_id: "call-1".to_owned(),
            },
            root: TrajectoryId::new(format!("receipt-root:{session_id}")),
        };

        let (lease, other) = (receipt_lease("a"), receipt_lease("b"));
        for scope in [&first, &second] {
            assert_eq!(
                store.claim_operation(operation(scope), &lease).unwrap(),
                OperationClaim::Claimed
            );
            assert_eq!(
                store.claim_processed_result(result(scope), &lease).unwrap(),
                ProcessedResultClaim::Claimed
            );
        }
        let decided = |scope: &SessionScope| serde_json::json!({"decision": scope.organization_id});
        store
            .complete_operation(operation(&first).key, decided(&first), lease.owner())
            .expect("the first organization completes its operation");
        store
            .complete_processed_result(result(&first).key, "first".to_owned(), decided(&first), lease.owner())
            .expect("the first organization completes its result");
        assert_eq!(
            store.claim_operation(operation(&second), &other).unwrap(),
            OperationClaim::Held
        );
        assert_eq!(
            store.claim_processed_result(result(&second), &other).unwrap(),
            ProcessedResultClaim::Held
        );
        store
            .complete_operation(operation(&second).key, decided(&second), lease.owner())
            .expect("the second organization completes its operation");
        store
            .complete_processed_result(
                result(&second).key,
                "second".to_owned(),
                decided(&second),
                lease.owner(),
            )
            .expect("the second organization completes its result");

        for (scope, output) in [(&first, "first"), (&second, "second")] {
            assert_eq!(
                store.claim_operation(operation(scope), &other).unwrap(),
                OperationClaim::Complete {
                    decision: decided(scope)
                }
            );
            assert_eq!(
                store.claim_processed_result(result(scope), &other).unwrap(),
                ProcessedResultClaim::Complete {
                    approved_output: output.to_owned(),
                    decision: decided(scope),
                }
            );
        }
    }

    #[test]
    fn memory_organizations_sharing_a_session_id_keep_their_receipts_apart() {
        organizations_sharing_a_session_id_keep_their_receipts_apart(&memory(), "shared-session");
    }

    #[test]
    fn sqlite_organizations_sharing_a_session_id_keep_their_receipts_apart() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let store = LogStore::open(Backend::Sqlite {
            path: dir.path().join("appa.db"),
        })
        .expect("a fresh store opens");
        organizations_sharing_a_session_id_keep_their_receipts_apart(&store, "shared-session");
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host receipt migrations"]
    fn postgres_organizations_sharing_a_session_id_keep_their_receipts_apart() {
        let store = postgres_store(1).lease().expect("the connection leases");
        let unique = tempfile::tempdir().expect("a unique session id exists");
        let session = format!("shared-session:{}", unique.path().display());
        organizations_sharing_a_session_id_keep_their_receipts_apart(&store, &session);
        store
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                client.execute("DELETE FROM openappa_operations WHERE session_id=$1", &[&session])?;
                client.execute(
                    "DELETE FROM openappa_processed_results WHERE session_id=$1",
                    &[&session],
                )?;
                Ok(())
            })
            .expect("the test receipts clean up");
    }

    /// Run against a disposable PostgreSQL database that holds the host schema,
    /// from a host's migrations or from `tests/fixtures/host_schema.sql`:
    /// OPENAPPA_TEST_DATABASE_URL=... cargo test -p appa-eventlog --features postgres,fault-injection -- --ignored
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_preserves_encoding_and_cas() {
        let first = postgres_store(2).lease().unwrap();
        let second = postgres_store(1);
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
        first.append(&before, &facts).unwrap();
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
            second.roots_mentioning("offer:one").unwrap().contains(&id),
            "the key names the root that recorded it"
        );
        assert_eq!(
            second.roots_mentioning("offer:two").unwrap(),
            Vec::new(),
            "and nothing for a key no record named"
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
                seq: before_vouch.basis(),
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
                .filter(
                    |result| matches!(result, Err(AppendError::Conflict { current }) if *current == stale.basis() + 1)
                )
                .count(),
            1
        );
        assert_eq!(second.log(&id).unwrap().basis(), stale.basis() + 1);

        let bound = HostObservation::CallBound {
            trajectory: id.clone(),
            call_id: "host-call-1".into(),
            dispatch: DispatchId::new(
                id.clone(),
                serde_json::from_value(serde_json::json!("00".repeat(32))).unwrap(),
                0,
            ),
            prompt: None,
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
        first.append_host(&before, &facts, &bound).unwrap();
        let restored = second.log(&id).unwrap();
        assert_eq!(bindings(&restored), expected);
        assert_eq!(restored.facts().len(), before.facts().len() + facts.len());
        assert!(matches!(
            second.append_host(&before, &facts, &bound),
            Err(AppendError::Conflict { current }) if current == before.basis() + 1
        ));

        first
            .postgres()
            .unwrap()
            .with_client(move |client| {
                client.execute("DELETE FROM openappa_events WHERE root=$1", &[&id.as_str()])?;
                client.execute("DELETE FROM openappa_host_keys WHERE root=$1", &[&id.as_str()])?;
                Ok(())
            })
            .unwrap();
    }

    /// Run against a host schema that includes the OpenAPPA integration
    /// receipts. The test leaves no rows behind and is intentionally opt-in:
    /// its contract is the typed host-storage API, not a shared development DB.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host receipt migrations"]
    fn postgres_typed_receipts_are_idempotent_and_fail_closed() {
        let store = postgres_store(1).lease().expect("the connection leases");
        let pg = store.postgres().expect("the PostgreSQL API is present");
        let unique = tempfile::tempdir().expect("a unique receipt namespace exists");
        let suffix = unique.path().display().to_string();
        let session = receipt_session(&suffix);
        let root = TrajectoryId::new(format!("receipt-root:{suffix}"));

        let request = OperationRequest {
            key: OperationKey {
                session: session.clone(),
                binding: caller_binding(),
                operation_id: "remedy-1".to_owned(),
            },
            root: root.clone(),
            input: serde_json::json!({"offer_id": "0123456789abcdef"}),
            context: None,
        };
        let (lease, other) = (receipt_lease("a"), receipt_lease("b"));
        assert!(matches!(
            store.claim_operation(request.clone(), &lease),
            Ok(OperationClaim::Claimed)
        ));
        assert!(matches!(
            store.claim_operation(request.clone(), &other),
            Ok(OperationClaim::Held)
        ));
        let decision = serde_json::json!({"decision":"mcp_result"});
        store
            .complete_operation(request.key.clone(), decision.clone(), lease.owner())
            .expect("the claimed operation completes");
        assert_eq!(
            store
                .claim_operation(request.clone(), &other)
                .expect("the completed operation replays"),
            OperationClaim::Complete {
                decision: decision.clone()
            }
        );
        let mut changed = request.clone();
        changed.input = serde_json::json!({"offer_id": "other"});
        assert!(matches!(
            store.claim_operation(changed, &lease),
            Err(ReceiptError::InputMismatch)
        ));

        let result = ProcessedResultRequest {
            key: ProcessedResultKey {
                session: session.clone(),
                caller_id: Some("caller".to_owned()),
                tool_call_id: "call-1".to_owned(),
            },
            root: root.clone(),
        };
        assert!(matches!(
            store.claim_processed_result(result.clone(), &lease),
            Ok(ProcessedResultClaim::Claimed)
        ));
        assert!(store.has_pending_receipts(&root).expect("the pending receipt checks"));
        store
            .complete_processed_result(
                result.key.clone(),
                "approved".to_owned(),
                decision.clone(),
                lease.owner(),
            )
            .expect("the processed result completes");
        assert_eq!(
            store
                .claim_processed_result(result, &other)
                .expect("the processed result replays"),
            ProcessedResultClaim::Complete {
                approved_output: "approved".to_owned(),
                decision,
            }
        );
        assert!(!store.has_pending_receipts(&root).expect("all receipts are terminal"));

        let session_bound = OperationRequest {
            key: OperationKey {
                session: session.clone(),
                binding: ReceiptBinding::Session { caller_id: None },
                operation_id: "remedy-callerless".to_owned(),
            },
            ..request.clone()
        };
        store
            .claim_operation(session_bound, &lease)
            .expect("the session-bound operation claims");
        let session_id = session.session_id.clone();
        let callers: Vec<(String, Option<String>)> = pg
            .with_client(move |client| {
                Ok(client
                    .query(
                        "SELECT operation_id, caller_id FROM openappa_operations WHERE session_id=$1 \
                         UNION ALL SELECT tool_call_id, caller_id FROM openappa_processed_results WHERE session_id=$1 \
                         ORDER BY 1",
                        &[&session_id],
                    )?
                    .iter()
                    .map(|row| (row.get(0), row.get(1)))
                    .collect())
            })
            .expect("the stored callers read");
        assert_eq!(
            callers,
            [
                ("call-1".to_owned(), Some("caller".to_owned())),
                ("remedy-1".to_owned(), Some("caller".to_owned())),
                ("remedy-callerless".to_owned(), None),
            ],
            "each receipt records the caller that claimed it"
        );

        pg.with_client(move |client| {
            client.execute(
                "DELETE FROM openappa_operations WHERE session_id=$1",
                &[&session.session_id],
            )?;
            client.execute(
                "DELETE FROM openappa_processed_results WHERE session_id=$1",
                &[&session.session_id],
            )?;
            Ok(())
        })
        .expect("the isolated test receipts clean up");
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_a_held_message_is_notified_once_and_taken_once() {
        let store = postgres_store(1);
        let unique = tempfile::tempdir().expect("a unique receiver name exists");
        let suffix = unique.path().display().to_string();
        held::tests::a_held_message_is_notified_once_and_taken_once(&store, &suffix);
        let receiver = format!("held-receiver:{suffix}");
        let remaining: i64 = store
            .lease()
            .expect("a connection leases")
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                Ok(client
                    .query_one(
                        "SELECT COUNT(*) FROM openappa_held_peer_messages WHERE receiver=$1",
                        &[&receiver],
                    )?
                    .get(0))
            })
            .expect("the held rows count");
        assert_eq!(remaining, 0, "every held row was taken");
    }

    /// The peer keys are written beside the record on this backend too.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_peer_keys_find_the_root_that_recorded_them() {
        let store = postgres_store(1);
        let root = postgres_root(&store, "peer");
        let observations = peer_observations(&root);
        for observation in &observations {
            store.append_host(&store.log(&root).unwrap(), &[], observation).unwrap();
        }
        for key in observations.iter().filter_map(HostObservation::key) {
            assert!(store.roots_mentioning(&key).unwrap().contains(&root), "{key}");
        }
        let cleaned = root.as_str().to_owned();
        store
            .lease()
            .unwrap()
            .postgres()
            .unwrap()
            .with_client(move |client| {
                client.execute("DELETE FROM openappa_host_keys WHERE root=$1", &[&cleaned])?;
                Ok(())
            })
            .unwrap();
        forget_postgres_roots(&store, vec![root]);
    }

    #[cfg(all(feature = "postgres", feature = "fault-injection"))]
    fn pool(store: &LogStore) -> &postgres::PostgresStore {
        match &store.store {
            Store::Postgres(pg) => pg,
            Store::Sqlite(_) => panic!("PostgreSQL-only operation"),
        }
    }

    #[cfg(feature = "postgres")]
    fn postgres_store(max_connections: usize) -> LogStore {
        let url = std::env::var("OPENAPPA_TEST_DATABASE_URL").expect("test database URL");
        LogStore::open(Backend::Postgres {
            url,
            max_connections: std::num::NonZeroUsize::new(max_connections).expect("a pool holds a connection"),
        })
        .expect("the PostgreSQL store opens")
    }

    #[cfg(feature = "postgres")]
    fn postgres_root(store: &LogStore, name: &str) -> TrajectoryId {
        let unique = tempfile::tempdir().expect("a unique root name exists");
        let id = TrajectoryId::new(format!("pg-test:{name}:{}", unique.path().display()));
        store
            .create_root(opening(&id), POLICY.as_bytes())
            .expect("the root opens");
        id
    }

    #[cfg(feature = "postgres")]
    fn forget_postgres_roots(store: &LogStore, roots: Vec<TrajectoryId>) {
        store
            .lease()
            .expect("a connection leases")
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                for root in &roots {
                    client.execute("DELETE FROM openappa_events WHERE root=$1", &[&root.as_str()])?;
                    client.execute("DELETE FROM openappa_roots WHERE root=$1", &[&root.as_str()])?;
                }
                Ok(())
            })
            .expect("the test roots clean up");
    }

    /// Whether a connection other than `store`'s can lock `root`'s family row right now.
    #[cfg(feature = "postgres")]
    fn postgres_root_is_free(store: &LogStore, root: &TrajectoryId) -> bool {
        let key = root.as_str().to_owned();
        store
            .lease()
            .expect("a connection leases")
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                let mut transaction = client.transaction()?;
                let free = match transaction.query_opt(
                    "SELECT 1 FROM openappa_roots WHERE root = $1 FOR UPDATE NOWAIT",
                    &[&key],
                ) {
                    Ok(row) => row.is_some(),
                    Err(error) if error.code() == Some(&::postgres::error::SqlState::LOCK_NOT_AVAILABLE) => false,
                    Err(error) => return Err(error.into()),
                };
                transaction.rollback()?;
                Ok(free)
            })
            .expect("the lock attempt answers")
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_a_step_holds_its_root_from_the_read_to_the_append() {
        let store = postgres_store(3);
        let root = postgres_root(&store, "step");
        let boundary = vec![Fact::Boundary {
            trajectory: root.clone(),
            kind: appa_engine::fact::BoundaryKind::VoidReturn,
        }];
        assert!(
            postgres_root_is_free(&store, &root),
            "an opened root has its row, and nobody holds it"
        );

        let step = store.step(&root).expect("the step begins");
        let log = step.log().expect("the step reads");
        assert!(!postgres_root_is_free(&store, &root), "the step holds the family's row");
        step.append(&log, &boundary).expect("the step appends");
        assert!(postgres_root_is_free(&store, &root), "the append ends the step");
        assert_eq!(store.log(&root).expect("the log reads").basis(), 2);

        let step = store.step(&root).expect("a second step begins");
        let _ = step.log().expect("the step reads");
        drop(step);
        assert!(postgres_root_is_free(&store, &root), "a dropped step lets the row go");
        assert_eq!(
            store.log(&root).expect("the log reads").basis(),
            2,
            "a dropped step wrote nothing"
        );
        forget_postgres_roots(&store, vec![root]);
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_concurrent_steps_of_one_family_all_append() {
        let writers = 12;
        let store = std::sync::Arc::new(postgres_store(writers + 1));
        let root = postgres_root(&store, "steps");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(writers));
        let handles: Vec<_> = (0..writers)
            .map(|_| {
                let (store, root, barrier) = (store.clone(), root.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let boundary = vec![Fact::Boundary {
                        trajectory: root.clone(),
                        kind: appa_engine::fact::BoundaryKind::VoidReturn,
                    }];
                    barrier.wait();
                    let step = store.step(&root).expect("the step begins");
                    let log = step.log().expect("the step reads");
                    step.append(&log, &boundary)
                })
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .expect("the writer finishes")
                .expect("no writer loses its append: the steps take turns");
        }
        assert_eq!(store.log(&root).expect("the log reads").basis(), writers as u64 + 1);
        forget_postgres_roots(&store, vec![root]);
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_a_cached_log_reads_new_batches_and_drops_a_rewritten_tail() {
        let reader = postgres_store(1);
        let writer = postgres_store(1);
        let root = postgres_root(&writer, "cached");
        let boundary = vec![Fact::Boundary {
            trajectory: root.clone(),
            kind: appa_engine::fact::BoundaryKind::VoidReturn,
        }];
        let first = reader.log(&root).expect("the log reads");
        writer
            .append(&writer.log(&root).expect("the log reads"), &boundary)
            .expect("another store appends");
        let second = reader.log(&root).expect("the log reads again");
        assert_eq!(second.basis(), first.basis() + 1, "the cached read sees the new batch");
        assert_eq!(
            second,
            writer.log(&root).expect("the writer reads"),
            "and equals a cold read"
        );

        // The last batch the reader cached is replaced behind its back.
        let key = root.as_str().to_owned();
        writer
            .lease()
            .expect("a connection leases")
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                client.execute(
                    "UPDATE openappa_events SET payload = $2 WHERE root = $1 AND seq = 1",
                    &[&key, &b"[]".to_vec()],
                )?;
                Ok(())
            })
            .expect("the batch is rewritten");
        let reread = reader.log(&root).expect("the rewritten log reads");
        assert_eq!(reread.basis(), 2);
        assert!(
            reread
                .batches_after(1)
                .expect("the batch exists")
                .all(<[Fact]>::is_empty),
            "the reader drops its cached copy and reads the stored batch"
        );
        forget_postgres_roots(&writer, vec![root]);
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_a_gapped_log_refuses_the_read_and_appends_after_its_highest_seq() {
        let store = postgres_store(1);
        let root = postgres_root(&store, "gapped");
        let boundary = vec![Fact::Boundary {
            trajectory: root.clone(),
            kind: appa_engine::fact::BoundaryKind::VoidReturn,
        }];
        for _ in 0..2 {
            let log = store.log(&root).expect("the log reads");
            store.append(&log, &boundary).expect("the append lands");
        }
        let before = store.log(&root).expect("the log reads");
        let gapped = root.as_str().to_owned();
        store
            .lease()
            .expect("a connection leases")
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                client.execute("DELETE FROM openappa_events WHERE root=$1 AND seq=1", &[&gapped])?;
                Ok(())
            })
            .expect("the middle row deletes");
        let error = store.log(&root).expect_err("a gapped log does not read");
        assert_eq!(StoreErrorClass::from(&error), StoreErrorClass::Storage, "{error:?}");
        store.append(&before, &boundary).expect("the append lands at seq 3");
        let listed = root.as_str().to_owned();
        let seqs: Vec<i64> = store
            .lease()
            .expect("a connection leases")
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                Ok(client
                    .query("SELECT seq FROM openappa_events WHERE root=$1 ORDER BY seq", &[&listed])?
                    .into_iter()
                    .map(|row| row.get(0))
                    .collect())
            })
            .expect("the seqs read");
        assert_eq!(seqs, [0, 2, 3]);
        forget_postgres_roots(&store, vec![root]);
    }

    #[cfg(feature = "postgres")]
    fn backend_pid(store: &LogStore) -> i32 {
        store
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(|client| Ok(client.query_one("SELECT pg_backend_pid()", &[])?.get(0)))
            .expect("the connection names its backend")
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_only_a_leased_store_runs_host_sql() {
        let store = postgres_store(1);
        assert!(
            store.postgres().is_none(),
            "a store without a connection of its own runs nothing a host's SQL would leave on one"
        );
        assert!(memory().postgres().is_none());
        let leased = store.lease().expect("the connection leases");
        assert!(
            leased
                .postgres()
                .expect("a leased store hands out its connection")
                .with_client(|client| Ok(client.query_one("SELECT 1", &[])?.get::<_, i32>(0)))
                .is_ok_and(|one| one == 1)
        );
    }

    /// A transaction host SQL leaves open on a lease ends when the connection returns to the
    /// pool: its writes roll back and the row locks it held are free. Two leases claiming one
    /// receipt contend on the receipt's own row.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host receipt migrations"]
    fn postgres_a_returned_connection_ends_its_transaction_and_receipts_contend_across_leases() {
        let store = postgres_store(2);
        let unique = tempfile::tempdir().expect("a unique namespace exists");
        let suffix = unique.path().display().to_string();
        let inbox = format!("lease-inbox:{suffix}");
        let (a, b) = (store.lease().unwrap(), store.lease().unwrap());
        let returned = backend_pid(&a);
        let held = inbox.clone();
        a.postgres()
            .unwrap()
            .with_client(move |client| {
                client.execute("INSERT INTO openappa_peer_inboxes (receiver) VALUES ($1)", &[&held])?;
                client.batch_execute("BEGIN")?;
                client.query_one(
                    "SELECT 1 FROM openappa_peer_inboxes WHERE receiver = $1 FOR UPDATE",
                    &[&held],
                )?;
                client.execute(
                    "INSERT INTO openappa_peer_inboxes (receiver) VALUES ($1)",
                    &[&format!("{held}:uncommitted")],
                )?;
                Ok(())
            })
            .expect("the host leaves a transaction open");
        let try_lock = |lease: &LogStore| {
            let receiver = inbox.clone();
            lease
                .postgres()
                .unwrap()
                .with_client(move |client| {
                    match client.query_one(
                        "SELECT 1 FROM openappa_peer_inboxes WHERE receiver = $1 FOR UPDATE NOWAIT",
                        &[&receiver],
                    ) {
                        Ok(_) => Ok(true),
                        Err(error) if error.code() == Some(&::postgres::error::SqlState::LOCK_NOT_AVAILABLE) => {
                            Ok(false)
                        }
                        Err(error) => Err(error.into()),
                    }
                })
                .expect("the lock attempt answers")
        };
        assert!(
            !try_lock(&b),
            "a row lock held in one lease's transaction excludes another lease"
        );
        drop(a);
        assert!(try_lock(&b), "a returned connection's transaction has ended");
        let a = store.lease().unwrap();
        assert_eq!(
            backend_pid(&a),
            returned,
            "the pool hands the returned connection out again"
        );
        let (fresh, uncommitted) = {
            let receiver = format!("{inbox}:uncommitted");
            a.postgres()
                .unwrap()
                .with_client(move |client| {
                    let row = client.query_one(
                        "SELECT txid_current_if_assigned() IS NULL, \
                                (SELECT COUNT(*) FROM openappa_peer_inboxes WHERE receiver = $1)",
                        &[&receiver],
                    )?;
                    Ok((row.get::<_, bool>(0), row.get::<_, i64>(1)))
                })
                .expect("the returned connection answers")
        };
        assert!(
            fresh,
            "the returned connection is no longer inside the transaction that wrote"
        );
        assert_eq!(uncommitted, 0, "what the open transaction wrote rolled back");
        drop((a, b));

        let request = OperationRequest {
            key: OperationKey {
                session: SessionScope {
                    organization_id: format!("lease-org:{suffix}"),
                    session_id: format!("lease-session:{suffix}"),
                },
                binding: ReceiptBinding::Session { caller_id: None },
                operation_id: "call:contended".to_owned(),
            },
            root: TrajectoryId::new(format!("lease-root:{suffix}")),
            input: serde_json::json!({"tool": "wire"}),
            context: None,
        };
        let (a, b) = (store.lease().unwrap(), store.lease().unwrap());
        let leases = [receipt_lease("a"), receipt_lease("b")];
        let barrier = std::sync::Barrier::new(2);
        let claims = std::thread::scope(|scope| {
            let (barrier, request) = (&barrier, &request);
            [(&a, &leases[0]), (&b, &leases[1])]
                .map(|(store, lease)| {
                    scope.spawn(move || {
                        barrier.wait();
                        store.claim_operation(request.clone(), lease)
                    })
                })
                .map(|claim| claim.join().unwrap())
        });
        let winner = claims
            .iter()
            .position(|claim| matches!(claim, Ok(OperationClaim::Claimed)))
            .expect("one lease claims");
        assert!(matches!(claims[1 - winner], Ok(OperationClaim::Held)), "{claims:?}");
        let decision = serde_json::json!({"decision": "allow_call"});
        [&a, &b][winner]
            .complete_operation(request.key.clone(), decision.clone(), leases[winner].owner())
            .expect("the claim completes");
        assert_eq!(
            [&a, &b][1 - winner]
                .claim_operation(request.clone(), &leases[1 - winner])
                .unwrap(),
            OperationClaim::Complete { decision },
            "the other lease replays what the first completed"
        );

        let session = request.key.session.session_id;
        a.postgres()
            .unwrap()
            .with_client(move |client| {
                client.execute("DELETE FROM openappa_operations WHERE session_id=$1", &[&session])?;
                client.execute("DELETE FROM openappa_peer_inboxes WHERE receiver = $1", &[&inbox])?;
                Ok(())
            })
            .expect("the test rows clean up");
    }

    /// One operation and one result request in a session of their own.
    #[cfg(feature = "postgres")]
    fn postgres_receipts() -> (OperationRequest, ProcessedResultRequest) {
        let suffix = uuid::Uuid::new_v4().to_string();
        let session = SessionScope {
            organization_id: format!("lease-org:{suffix}"),
            session_id: format!("lease-session:{suffix}"),
        };
        let root = TrajectoryId::new(format!("lease-root:{suffix}"));
        let operation = OperationRequest {
            key: OperationKey {
                session: session.clone(),
                binding: caller_binding(),
                operation_id: "op-1".to_owned(),
            },
            root: root.clone(),
            input: serde_json::json!({"tool": "wire"}),
            context: None,
        };
        let result = ProcessedResultRequest {
            key: ProcessedResultKey {
                session,
                caller_id: Some("caller".to_owned()),
                tool_call_id: "call-1".to_owned(),
            },
            root,
        };
        (operation, result)
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host receipt migrations"]
    fn postgres_plain_results_replay_verbatim_after_an_upgrade() {
        let store = postgres_store(1);
        let (_, mut result) = postgres_receipts();
        let root = postgres_root(&store, "plaintext-upgrade");
        result.root = root.clone();
        let lease = receipt_lease("plain");
        let decision = serde_json::json!({"decision": "ack"});
        let output = "appa-storage-v1:user content";
        store.claim_processed_result(result.clone(), &lease).unwrap();
        store
            .complete_processed_result(result.key.clone(), output.to_owned(), decision.clone(), lease.owner())
            .unwrap();
        // The previous implementation stored approved_output verbatim. Set it
        // directly so this also exercises rows written by that implementation.
        let session = result.key.session.session_id.clone();
        let stored_session = session.clone();
        store
            .lease()
            .unwrap()
            .postgres()
            .unwrap()
            .with_client(move |client| {
                client.execute(
                    "UPDATE openappa_processed_results SET approved_output=$2 WHERE session_id=$1",
                    &[&stored_session, &output],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.claim_processed_result(result, &lease).unwrap(),
            ProcessedResultClaim::Complete {
                approved_output: output.to_owned(),
                decision
            }
        );
        forget_postgres_receipts(&store, &session);
        forget_postgres_roots(&store, vec![root]);
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host receipt migrations"]
    fn postgres_encrypted_result_completion_requires_the_root_key() {
        let plain = postgres_store(1);
        let cipher = encryption::StorageCipher::encrypted(&[7; 32], "encrypted-result".into()).unwrap();
        let encrypted = plain.encrypted_view(cipher).unwrap();
        let root = postgres_root(&encrypted, "encrypted-result");
        let (_, mut result) = postgres_receipts();
        result.root = root.clone();
        let session = result.key.session.session_id.clone();
        let lease = receipt_lease("encrypted");
        let decision = serde_json::json!({"decision": "ack"});
        encrypted.claim_processed_result(result.clone(), &lease).unwrap();
        encrypted
            .complete_processed_result(
                result.key.clone(),
                "private output".into(),
                decision.clone(),
                lease.owner(),
            )
            .unwrap();
        let error = plain
            .complete_processed_result(
                result.key.clone(),
                "private output".into(),
                decision.clone(),
                lease.owner(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("requires its conversation key"));
        let wrong = plain
            .encrypted_view(encryption::StorageCipher::encrypted(&[8; 32], "encrypted-result".into()).unwrap())
            .unwrap();
        let error = wrong
            .complete_processed_result(
                result.key.clone(),
                "private output".into(),
                decision.clone(),
                lease.owner(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("authentication failed"));
        assert_eq!(
            encrypted.claim_processed_result(result, &lease).unwrap(),
            ProcessedResultClaim::Complete {
                approved_output: "private output".into(),
                decision
            }
        );
        forget_postgres_receipts(&encrypted, &session);
        forget_postgres_roots(&encrypted, vec![root]);
    }

    /// Run `sql` with the session id as `$1`, on a connection of its own.
    #[cfg(feature = "postgres")]
    fn postgres_session_sql(store: &LogStore, sql: &'static str, session: &str) {
        let session = session.to_owned();
        store
            .lease()
            .expect("a connection leases")
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                client.batch_execute("BEGIN")?;
                client.execute(sql, &[&session])?;
                client.batch_execute("COMMIT")?;
                Ok(())
            })
            .expect("the statement runs");
    }

    /// Run every lease in the session out, by the database clock.
    #[cfg(feature = "postgres")]
    fn lapse_postgres(store: &LogStore, session: &str) {
        postgres_session_sql(
            store,
            "WITH operations AS ( \
                 UPDATE openappa_operations SET lease_expires_at = now() - interval '1 hour' WHERE session_id = $1 \
             ) \
             UPDATE openappa_processed_results SET lease_expires_at = now() - interval '1 hour' WHERE session_id = $1",
            session,
        );
    }

    #[cfg(feature = "postgres")]
    fn forget_postgres_receipts(store: &LogStore, session: &str) {
        postgres_session_sql(
            store,
            "WITH operations AS (DELETE FROM openappa_operations WHERE session_id = $1) \
             DELETE FROM openappa_processed_results WHERE session_id = $1",
            session,
        );
    }

    /// The lease on the session's `op-1` and `call-1`, in that order: owner, and whether it
    /// still runs by the database clock.
    #[cfg(feature = "postgres")]
    fn postgres_leases(store: &LogStore, session: &str) -> Vec<(Option<String>, bool)> {
        let session = session.to_owned();
        store
            .lease()
            .expect("a connection leases")
            .postgres()
            .expect("the PostgreSQL API is present")
            .with_client(move |client| {
                Ok(client
                    .query(
                        "SELECT 1, lease_owner, COALESCE(lease_expires_at > now(), false) FROM openappa_operations \
                         WHERE session_id = $1 AND operation_id = 'op-1' \
                         UNION ALL \
                         SELECT 2, lease_owner, COALESCE(lease_expires_at > now(), false) FROM openappa_processed_results \
                         WHERE session_id = $1 AND tool_call_id = 'call-1' \
                         ORDER BY 1",
                        &[&session],
                    )?
                    .iter()
                    .map(|row| (row.get(1), row.get(2)))
                    .collect())
            })
            .expect("the leases read")
    }

    /// A live lease holds every other owner off; its holder claims again even after it ran
    /// out, since nobody displaced it; and once it has run out by the database clock, the
    /// next owner recovers the receipt.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host receipt migrations"]
    fn postgres_a_lease_holds_off_other_owners_until_it_lapses() {
        let store = postgres_store(2);
        let (operation, result) = postgres_receipts();
        let session = operation.key.session.session_id.clone();
        let (a, b) = (receipt_lease("a"), receipt_lease("b"));
        assert_eq!(
            store.claim_operation(operation.clone(), &a).unwrap(),
            OperationClaim::Claimed
        );
        assert_eq!(
            store.claim_processed_result(result.clone(), &a).unwrap(),
            ProcessedResultClaim::Claimed
        );
        assert_eq!(
            store.claim_operation(operation.clone(), &b).unwrap(),
            OperationClaim::Held
        );
        assert_eq!(
            store.claim_processed_result(result.clone(), &b).unwrap(),
            ProcessedResultClaim::Held
        );
        assert_eq!(
            postgres_leases(&store, &session),
            [(Some("a".to_owned()), true), (Some("a".to_owned()), true)],
            "a held claim writes nothing"
        );

        lapse_postgres(&store, &session);
        assert_eq!(
            store.claim_operation(operation.clone(), &a).unwrap(),
            OperationClaim::Claimed
        );
        assert_eq!(
            store.claim_processed_result(result.clone(), &a).unwrap(),
            ProcessedResultClaim::Claimed
        );
        assert_eq!(
            postgres_leases(&store, &session),
            [(Some("a".to_owned()), true), (Some("a".to_owned()), true)],
            "the holder's claim extends its lease"
        );

        lapse_postgres(&store, &session);
        assert_eq!(
            store.claim_operation(operation.clone(), &b).unwrap(),
            OperationClaim::Recovered
        );
        assert_eq!(
            store.claim_processed_result(result.clone(), &b).unwrap(),
            ProcessedResultClaim::Recovered
        );
        assert_eq!(
            postgres_leases(&store, &session),
            [(Some("b".to_owned()), true), (Some("b".to_owned()), true)],
            "a recovery takes the lease over"
        );
        assert_eq!(
            store.claim_operation(operation.clone(), &a).unwrap(),
            OperationClaim::Held,
            "the displaced owner is held off in turn"
        );

        // A brief lease runs out on the database clock alone.
        let brief = ReceiptLease::new(lease_owner("brief"), Duration::from_millis(200)).expect("the lease is valid");
        let mut briefly = operation.clone();
        briefly.key.operation_id = "op-brief".to_owned();
        assert_eq!(
            store.claim_operation(briefly.clone(), &brief).unwrap(),
            OperationClaim::Claimed
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            match store.claim_operation(briefly.clone(), &b).unwrap() {
                OperationClaim::Recovered => break,
                OperationClaim::Held if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                other => panic!("the brief lease never ran out: {other:?}"),
            }
        }

        // A pending row written without a lease has no time left.
        postgres_session_sql(
            &store,
            "INSERT INTO openappa_operations (organization_id, caller_id, session_id, operation_id, root, input, status) \
             SELECT organization_id, caller_id, session_id, 'op-unleased', root, input, 'pending' \
             FROM openappa_operations WHERE session_id = $1 AND operation_id = 'op-1'",
            &session,
        );
        let mut unleased = operation.clone();
        unleased.key.operation_id = "op-unleased".to_owned();
        assert_eq!(store.claim_operation(unleased, &a).unwrap(), OperationClaim::Recovered);
        forget_postgres_receipts(&store, &session);
    }

    /// Once another owner recovered a receipt, the owner it displaced can neither complete nor
    /// renew it, even once the new owner completed it. A completed receipt accepts its own
    /// completion from anyone.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host receipt migrations"]
    fn postgres_a_displaced_owner_can_neither_complete_nor_renew() {
        let store = postgres_store(2);
        let (operation, result) = postgres_receipts();
        let session = operation.key.session.session_id.clone();
        let (a, b) = (receipt_lease("a"), receipt_lease("b"));
        let decision = serde_json::json!({"decision": "allow_call"});
        store.claim_operation(operation.clone(), &a).unwrap();
        store.claim_processed_result(result.clone(), &a).unwrap();
        assert_eq!(
            store
                .renew_receipts(
                    &a,
                    std::slice::from_ref(&operation.key),
                    std::slice::from_ref(&result.key)
                )
                .unwrap(),
            Renewal::Kept
        );
        lapse_postgres(&store, &session);
        assert_eq!(
            store.claim_operation(operation.clone(), &b).unwrap(),
            OperationClaim::Recovered
        );
        assert_eq!(
            store.claim_processed_result(result.clone(), &b).unwrap(),
            ProcessedResultClaim::Recovered
        );

        assert!(matches!(
            store.complete_operation(operation.key.clone(), decision.clone(), a.owner()),
            Err(ReceiptError::LeaseLost)
        ));
        assert!(matches!(
            store.complete_processed_result(result.key.clone(), "approved".to_owned(), decision.clone(), a.owner()),
            Err(ReceiptError::LeaseLost)
        ));
        assert_eq!(
            store
                .renew_receipts(&a, std::slice::from_ref(&operation.key), &[])
                .unwrap(),
            Renewal::Lost
        );
        assert_eq!(
            store
                .renew_receipts(&a, &[], std::slice::from_ref(&result.key))
                .unwrap(),
            Renewal::Lost
        );
        assert_eq!(
            postgres_leases(&store, &session),
            [(Some("b".to_owned()), true), (Some("b".to_owned()), true)],
            "a lost renewal extends nothing of another owner's"
        );
        assert_eq!(
            store
                .renew_receipts(
                    &b,
                    std::slice::from_ref(&operation.key),
                    std::slice::from_ref(&result.key)
                )
                .unwrap(),
            Renewal::Kept
        );

        let completed = |store: &LogStore| -> bool {
            let session = session.clone();
            store
                .lease()
                .unwrap()
                .postgres()
                .unwrap()
                .with_client(move |client| {
                    Ok(client
                        .query_one(
                            "SELECT completed_at IS NOT NULL FROM openappa_processed_results \
                             WHERE session_id = $1 AND tool_call_id = 'call-1'",
                            &[&session],
                        )?
                        .get(0))
                })
                .expect("the completion time reads")
        };
        assert!(!completed(&store), "a pending result has no completion time");
        store
            .complete_operation(operation.key.clone(), decision.clone(), b.owner())
            .expect("the new owner completes");
        store
            .complete_processed_result(result.key.clone(), "approved".to_owned(), decision.clone(), b.owner())
            .expect("the new owner completes");
        assert!(completed(&store), "a completed result records when it completed");
        store
            .complete_operation(operation.key.clone(), decision.clone(), a.owner())
            .expect("a completed receipt accepts its own completion from anyone");
        assert!(matches!(
            store.complete_operation(
                operation.key.clone(),
                serde_json::json!({"decision": "deny"}),
                a.owner()
            ),
            Err(ReceiptError::CompletionMismatch)
        ));
        assert_eq!(
            store
                .renew_receipts(
                    &a,
                    std::slice::from_ref(&operation.key),
                    std::slice::from_ref(&result.key)
                )
                .unwrap(),
            Renewal::Lost,
            "a receipt another owner took over and completed stays lost to the displaced one"
        );
        assert_eq!(
            store
                .renew_receipts(
                    &b,
                    std::slice::from_ref(&operation.key),
                    std::slice::from_ref(&result.key)
                )
                .unwrap(),
            Renewal::Kept,
            "a receipt its owner completed is kept"
        );
        let mut missing = operation.key.clone();
        missing.operation_id = "op-missing".to_owned();
        assert_eq!(store.renew_receipts(&a, &[missing], &[]).unwrap(), Renewal::Lost);
        assert!(!store.has_pending_receipts(&operation.root).unwrap());
        forget_postgres_receipts(&store, &session);
    }

    /// A release ends the caller's own leases by the database clock and keeps the rows, so the
    /// next claim recovers them at once. Completed receipts and other owners' leases are
    /// untouched.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host receipt migrations"]
    fn postgres_a_release_lets_the_next_claim_recover_at_once() {
        let store = postgres_store(2);
        let (operation, result) = postgres_receipts();
        let session = operation.key.session.session_id.clone();
        let (a, b, c) = (receipt_lease("a"), receipt_lease("b"), receipt_lease("c"));
        let keyed = |id: &str| {
            let mut request = operation.clone();
            request.key.operation_id = id.to_owned();
            request.context = Some(serde_json::json!({"hook": "pre"}));
            request
        };
        let (mine, theirs, done) = (keyed("op-mine"), keyed("op-theirs"), keyed("op-done"));
        store.claim_operation(mine.clone(), &a).unwrap();
        store.claim_operation(theirs.clone(), &b).unwrap();
        store.claim_operation(done.clone(), &a).unwrap();
        let decision = serde_json::json!({"decision": "allow_call"});
        store
            .complete_operation(done.key.clone(), decision.clone(), a.owner())
            .unwrap();
        let mut result_theirs = result.clone();
        result_theirs.key.tool_call_id = "call-theirs".to_owned();
        store.claim_processed_result(result.clone(), &a).unwrap();
        store.claim_processed_result(result_theirs.clone(), &b).unwrap();

        store
            .release_receipts(
                a.owner(),
                &[done.key.clone(), theirs.key.clone(), mine.key.clone()],
                &[result_theirs.key.clone(), result.key.clone()],
            )
            .expect("the release lands");
        /// One receipt row: its id, status, lease owner, whether that lease still runs, and the
        /// context its recorded input carries.
        type Released = (String, String, Option<String>, bool, Option<serde_json::Value>);
        let listed = session.clone();
        let remaining: Vec<Released> = store
            .lease()
            .unwrap()
            .postgres()
            .unwrap()
            .with_client(move |client| {
                Ok(client
                    .query(
                        "SELECT operation_id, status, lease_owner, COALESCE(lease_expires_at > now(), false), \
                                input -> 'context' \
                         FROM openappa_operations WHERE session_id = $1 \
                         UNION ALL \
                         SELECT tool_call_id, status, lease_owner, COALESCE(lease_expires_at > now(), false), NULL \
                         FROM openappa_processed_results WHERE session_id = $1 \
                         ORDER BY 1",
                        &[&listed],
                    )?
                    .iter()
                    .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4)))
                    .collect())
            })
            .expect("the receipts read");
        let hook = Some(serde_json::json!({"hook": "pre"}));
        let row = |id: &str, status: &str, holder: &str, live: bool, context: &Option<serde_json::Value>| {
            (
                id.to_owned(),
                status.to_owned(),
                Some(holder.to_owned()),
                live,
                context.clone(),
            )
        };
        assert_eq!(
            remaining,
            [
                row("call-1", "pending", "a", false, &None),
                row("call-theirs", "pending", "b", true, &None),
                row("op-done", "complete", "a", true, &hook),
                row("op-mine", "pending", "a", false, &hook),
                row("op-theirs", "pending", "b", true, &hook),
            ],
            "a release ends the caller's pending leases and deletes nothing"
        );
        assert_eq!(
            store.claim_operation(mine, &c).unwrap(),
            OperationClaim::Recovered,
            "released work is recovered at once"
        );
        assert_eq!(
            store.claim_processed_result(result, &c).unwrap(),
            ProcessedResultClaim::Recovered
        );
        assert_eq!(
            store.claim_operation(theirs, &c).unwrap(),
            OperationClaim::Held,
            "another owner's lease is untouched"
        );
        assert_eq!(
            store.claim_processed_result(result_theirs, &c).unwrap(),
            ProcessedResultClaim::Held
        );
        assert_eq!(
            store.claim_operation(done, &c).unwrap(),
            OperationClaim::Complete { decision }
        );
        forget_postgres_receipts(&store, &session);
    }

    /// Owners racing to claim one receipt from leases of their own: the primary key admits one
    /// row, so exactly one claims and every other is held off.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host receipt migrations"]
    fn postgres_concurrent_claims_admit_exactly_one_owner() {
        const OWNERS: usize = 8;
        let store = postgres_store(OWNERS);
        let (operation, result) = postgres_receipts();
        let session = operation.key.session.session_id.clone();
        let leased: Vec<LogStore> = (0..OWNERS)
            .map(|_| store.lease().expect("a connection leases"))
            .collect();
        let leases: Vec<ReceiptLease> = (0..OWNERS)
            .map(|index| receipt_lease(&format!("owner-{index}")))
            .collect();
        let barrier = std::sync::Barrier::new(OWNERS);
        let (operations, results): (Vec<_>, Vec<_>) = std::thread::scope(|scope| {
            let racers: Vec<_> = leased
                .iter()
                .zip(&leases)
                .map(|(store, lease)| {
                    let (barrier, operation, result) = (&barrier, &operation, &result);
                    scope.spawn(move || {
                        barrier.wait();
                        let claimed = store.claim_operation(operation.clone(), lease).unwrap();
                        barrier.wait();
                        (claimed, store.claim_processed_result(result.clone(), lease).unwrap())
                    })
                })
                .collect();
            racers
                .into_iter()
                .map(|racer| racer.join().expect("the racer finishes"))
                .unzip()
        });
        let count = |claims: &[OperationClaim], wanted: &OperationClaim| claims.iter().filter(|c| *c == wanted).count();
        assert_eq!(count(&operations, &OperationClaim::Claimed), 1, "{operations:?}");
        assert_eq!(count(&operations, &OperationClaim::Held), OWNERS - 1, "{operations:?}");
        let count = |claims: &[ProcessedResultClaim], wanted: &ProcessedResultClaim| {
            claims.iter().filter(|c| *c == wanted).count()
        };
        assert_eq!(count(&results, &ProcessedResultClaim::Claimed), 1, "{results:?}");
        assert_eq!(count(&results, &ProcessedResultClaim::Held), OWNERS - 1, "{results:?}");
        drop(leased);
        forget_postgres_receipts(&store, &session);
    }

    /// Holders racing on one receiver serialize on its inbox row, so the quota trims exactly.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_held_peer_quota_holds_under_concurrent_holds() {
        const HOLDERS: usize = 8;
        const EACH: usize = 4;
        const QUOTA: usize = 5;
        let store = postgres_store(HOLDERS);
        let receiver = TrajectoryId::new(format!("held-quota:{}", uuid::Uuid::new_v4()));
        let leased: Vec<LogStore> = (0..HOLDERS)
            .map(|_| store.lease().expect("a connection leases"))
            .collect();
        let now = SystemTime::now();
        let barrier = std::sync::Barrier::new(HOLDERS);
        std::thread::scope(|scope| {
            for (holder, store) in leased.iter().enumerate() {
                let (barrier, receiver) = (&barrier, &receiver);
                scope.spawn(move || {
                    barrier.wait();
                    for index in 0..EACH {
                        let body = format!("{holder}:{index}");
                        store
                            .hold_peer_message(
                                receiver,
                                PeerDigest::of_body(&body),
                                &Label::top(),
                                &body,
                                now,
                                Duration::from_secs(600),
                                QUOTA,
                            )
                            .expect("the message is held");
                    }
                });
            }
        });
        drop(leased);
        assert_eq!(
            store.peer_notices(&receiver, now).expect("the notices read").len(),
            QUOTA,
            "concurrent holds never leave the receiver over its quota"
        );
        let cleaned = receiver.as_str().to_owned();
        store
            .lease()
            .unwrap()
            .postgres()
            .unwrap()
            .with_client(move |client| {
                let remaining: i64 = client
                    .query_one(
                        "SELECT COUNT(*) FROM openappa_held_peer_messages WHERE receiver = $1",
                        &[&cleaned],
                    )?
                    .get(0);
                client.execute(
                    "DELETE FROM openappa_held_peer_messages WHERE receiver = $1",
                    &[&cleaned],
                )?;
                client.execute("DELETE FROM openappa_peer_inboxes WHERE receiver = $1", &[&cleaned])?;
                assert_eq!(remaining, QUOTA as i64);
                Ok(())
            })
            .expect("the held rows clean up");
    }

    /// Senders racing into one family serialize on its root row, so the unread quota holds;
    /// every other embedded write runs under the same lock.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_embedded_peer_quota_holds_under_concurrent_sends() {
        const SENDERS: usize = 8;
        const OPEN: usize = 2;
        let store = postgres_store(SENDERS);
        let root = TrajectoryId::new(format!("embedded-root:{}", uuid::Uuid::new_v4()));
        let (sender, recipient) = (
            TrajectoryId::new("embedded-sender"),
            TrajectoryId::new("embedded-recipient"),
        );
        let send = |store: &LogStore, dispatch: String| {
            store
                .claim_embedded_peer(
                    &root,
                    &sender,
                    &recipient,
                    None,
                    &dispatch,
                    &PeerDigest::of_body(&dispatch),
                    &Label::top(),
                    &dispatch,
                    SystemTime::now(),
                )
                .expect("the send answers")
        };
        for index in 0..embedded::UNREAD_QUOTA - OPEN {
            assert!(matches!(
                send(&store, format!("dispatch-{index}")),
                EmbeddedClaim::Stored(_)
            ));
        }
        let leased: Vec<LogStore> = (0..SENDERS)
            .map(|_| store.lease().expect("a connection leases"))
            .collect();
        let barrier = std::sync::Barrier::new(SENDERS);
        let claims: Vec<EmbeddedClaim> = std::thread::scope(|scope| {
            let racers: Vec<_> = leased
                .iter()
                .enumerate()
                .map(|(index, store)| {
                    let (barrier, send) = (&barrier, &send);
                    scope.spawn(move || {
                        barrier.wait();
                        send(store, format!("race-{index}"))
                    })
                })
                .collect();
            racers.into_iter().map(|racer| racer.join().unwrap()).collect()
        });
        drop(leased);
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, EmbeddedClaim::Stored(_)))
                .count(),
            OPEN,
            "{claims:?}"
        );
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, EmbeddedClaim::Quota))
                .count(),
            SENDERS - OPEN
        );
        let inbox = store
            .list_embedded_peer(&root, &recipient, SystemTime::now())
            .expect("the inbox lists");
        assert_eq!(inbox.len(), embedded::UNREAD_QUOTA);

        let (read, direct) = (&inbox[0], &inbox[1]);
        let ReadTake::Ready(claim) = store
            .claim_embedded_read(&root, &read.id, &recipient, "call-read", "{}")
            .expect("the read claims")
        else {
            panic!("the held message is ready to read");
        };
        assert!(
            store
                .release_embedded_read(&root, &read.id, "call-read", claim.generation)
                .expect("the release answers")
        );
        let ReadTake::Ready(claim) = store
            .claim_embedded_read(&root, &read.id, &recipient, "call-read", "{}")
            .expect("the read claims again")
        else {
            panic!("the released message is ready to read again");
        };
        assert!(
            store
                .finish_embedded_read(&root, &read.id, "call-read", claim.generation, r#"{"kind":"finished"}"#)
                .expect("the finish answers")
        );
        assert!(matches!(
            store
                .take_embedded_direct(
                    &root,
                    &direct.id,
                    &sender,
                    &recipient,
                    &PeerDigest::parse(&direct.digest).expect("the digest parses")
                )
                .expect("the direct take answers"),
            DirectTake::Taken(_)
        ));

        let cleaned = root.as_str().to_owned();
        store
            .lease()
            .unwrap()
            .postgres()
            .unwrap()
            .with_client(move |client| {
                client.execute(
                    "DELETE FROM openappa_embedded_peer_messages WHERE root = $1",
                    &[&cleaned],
                )?;
                client.execute("DELETE FROM openappa_roots WHERE root = $1", &[&cleaned])?;
                Ok(())
            })
            .expect("the embedded rows clean up");
    }

    /// The workspace position is the primary key: of appends racing at one basis exactly one
    /// lands, and a loser's root binding rolls back with it. A bound root refuses another
    /// workspace.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_a_stale_workspace_append_conflicts_and_a_root_binds_one_workspace() {
        const WRITERS: usize = 4;
        let store = postgres_store(WRITERS);
        let suffix = uuid::Uuid::new_v4().to_string();
        let (workspace, other) = (format!("pg-workspace:{suffix}"), format!("pg-workspace-other:{suffix}"));
        for opened in [&workspace, &workspace, &other] {
            store
                .open_workspace(opened, "policy", &Label::top())
                .expect("the workspace opens");
        }
        let stale = store.workspace_log(&workspace).expect("the workspace reads");
        assert_eq!(stale.basis, 1, "a second opening writes nothing");
        let roots: Vec<TrajectoryId> = (0..WRITERS)
            .map(|index| TrajectoryId::new(format!("pg-workspace-root:{index}:{suffix}")))
            .collect();
        let leased: Vec<LogStore> = (0..WRITERS)
            .map(|_| store.lease().expect("a connection leases"))
            .collect();
        let barrier = std::sync::Barrier::new(WRITERS);
        let appended: Vec<_> = std::thread::scope(|scope| {
            let racers: Vec<_> = leased
                .iter()
                .zip(&roots)
                .map(|(store, root)| {
                    let (barrier, stale) = (&barrier, &stale);
                    scope.spawn(move || {
                        barrier.wait();
                        store.append_workspace(stale, &files::WorkspaceEvent::RootBound { root: root.clone() })
                    })
                })
                .collect();
            racers.into_iter().map(|racer| racer.join().unwrap()).collect()
        });
        drop(leased);
        let winner = appended.iter().position(Result::is_ok).expect("one append lands");
        for (index, outcome) in appended.iter().enumerate().filter(|(index, _)| *index != winner) {
            assert!(
                matches!(outcome, Err(files::FileStoreError::Conflict { expected: 1, actual: 2 })),
                "{index}: {outcome:?}"
            );
            assert_eq!(
                store.workspace_for_root(&roots[index]).unwrap(),
                None,
                "a losing append's root binding rolled back"
            );
        }
        assert_eq!(
            store.workspace_for_root(&roots[winner]).unwrap(),
            Some(workspace.clone())
        );
        store
            .bind_workspace_root(&workspace, &roots[winner])
            .expect("an identical binding is idempotent");

        let other_log = store.workspace_log(&other).expect("the other workspace reads");
        assert!(matches!(
            store.append_workspace(
                &other_log,
                &files::WorkspaceEvent::RootBound {
                    root: roots[winner].clone()
                }
            ),
            Err(files::FileStoreError::Configuration(_))
        ));
        assert_eq!(
            store.workspace_log(&other).expect("the other workspace reads").basis,
            1,
            "a refused binding writes no event"
        );

        store
            .lease()
            .unwrap()
            .postgres()
            .unwrap()
            .with_client(move |client| {
                for cleaned in [&workspace, &other] {
                    client.execute("DELETE FROM openappa_file_events WHERE workspace = $1", &[cleaned])?;
                    client.execute("DELETE FROM openappa_file_roots WHERE workspace = $1", &[cleaned])?;
                }
                Ok(())
            })
            .expect("the workspace rows clean up");
    }

    /// A host whose receipt tables are keyed without the organization, or with its columns in
    /// another order, refuses to open rather than let organizations collide on a receipt.
    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_refuses_a_host_whose_receipt_keys_omit_the_organization() {
        let url = std::env::var("OPENAPPA_TEST_DATABASE_URL").expect("test database URL");
        let fixture = include_str!("../tests/fixtures/host_schema.sql");
        let unique = tempfile::tempdir().expect("a unique schema name exists");
        let suffix: String = unique
            .path()
            .display()
            .to_string()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_lowercase();
        let hosts = [
            ("current", fixture.to_owned(), true),
            (
                "unscoped",
                fixture
                    .replace(
                        "(organization_id, session_id, operation_id)",
                        "(session_id, operation_id)",
                    )
                    .replace(
                        "(organization_id, session_id, tool_call_id)",
                        "(session_id, tool_call_id)",
                    ),
                false,
            ),
            (
                "reordered",
                fixture.replace(
                    "(organization_id, session_id, tool_call_id)",
                    "(session_id, organization_id, tool_call_id)",
                ),
                false,
            ),
        ];
        let mut admin = ::postgres::Client::connect(&url, ::postgres::NoTls).expect("the admin connection opens");
        for (name, ddl, opens) in hosts {
            let schema = format!("appa_probe_{name}_{suffix}");
            admin
                .batch_execute(&format!("CREATE SCHEMA {schema}; SET search_path TO {schema}; {ddl}"))
                .expect("the probe schema installs");
            let separator = if url.contains('?') { '&' } else { '?' };
            let opened = LogStore::open(Backend::Postgres {
                url: format!("{url}{separator}options=-c%20search_path%3D{schema}"),
                max_connections: std::num::NonZeroUsize::new(1).expect("a pool holds a connection"),
            });
            admin
                .batch_execute(&format!("DROP SCHEMA {schema} CASCADE; RESET search_path"))
                .expect("the probe schema drops");
            assert_eq!(opened.is_ok(), opens, "{name}");
        }
    }

    #[cfg(feature = "postgres")]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_pool_replaces_a_terminated_connection() {
        let terminate = |pid: i32| {
            postgres_store(1)
                .lease()
                .unwrap()
                .postgres()
                .unwrap()
                .with_client(move |client| {
                    client.execute("SELECT pg_terminate_backend($1)", &[&pid])?;
                    Ok(())
                })
                .expect("the server ends the connection");
        };
        let store = postgres_store(1);
        let root = postgres_root(&store, "terminated");
        let lease = store.lease().unwrap();
        terminate(backend_pid(&lease));
        assert!(lease.log(&root).is_err(), "work on a dead connection fails closed");
        drop(lease);
        assert!(
            store
                .has_root(&root)
                .expect("the pool opens a connection in the dead one's place"),
            "the store serves again without reopening"
        );

        // The same end while the connection sits idle costs no operation at all.
        let idle = backend_pid(&store.lease().unwrap());
        terminate(idle);
        // Past the window in which a connection that just came back is leased unasked.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(
            store
                .has_root(&root)
                .expect("the pool tells a dead idle connection from a live one"),
        );
        assert_ne!(backend_pid(&store.lease().unwrap()), idle);
        forget_postgres_roots(&store, vec![root]);
    }

    #[cfg(all(feature = "postgres", feature = "fault-injection"))]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_pool_bounds_its_checkout_and_its_reset() {
        use std::time::Duration;

        // The checkout wait also bounds opening a replacement connection, so it must fit a
        // real connect; the reset wait only has to stay well under the armed stall.
        let checkout = Duration::from_secs(5);
        let reset = Duration::from_millis(200);
        let store = postgres_store(1);
        let pg = pool(&store);
        pg.set_waits(checkout, reset);
        let lease = store.lease().unwrap();
        let pid = backend_pid(&lease);
        let asked = std::time::Instant::now();
        assert!(
            matches!(store.lease(), Err(crate::postgres::LeaseError::Exhausted(wait)) if wait == checkout),
            "a full pool refuses within its wait instead of hanging"
        );
        let waited = asked.elapsed();
        assert!(
            waited >= checkout && waited < checkout + Duration::from_secs(5),
            "the refusal comes when the wait runs out: {waited:?}"
        );

        pg.stall_next_reset(reset * 10);
        drop(lease);
        assert_ne!(
            backend_pid(&store.lease().expect("a silent connection frees its place")),
            pid,
            "a connection that does not answer its reset is never handed out again"
        );
    }

    /// A replacement connection opened late in a checkout gets only what is left of the
    /// checkout wait, so one checkout never takes much longer than that wait.
    #[cfg(all(feature = "postgres", feature = "fault-injection"))]
    #[test]
    #[ignore = "requires OPENAPPA_TEST_DATABASE_URL and host migrations"]
    fn postgres_a_replacement_connection_gets_only_the_remaining_checkout_wait() {
        use std::time::{Duration, Instant};

        let checkout = Duration::from_secs(2);
        let reset = Duration::from_millis(100);
        let store = postgres_store(1);
        let pg = pool(&store);
        pg.set_waits(checkout, reset);
        let lease = store.lease().unwrap();
        let (leased, waited) = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let asked = Instant::now();
                (store.lease().map(drop), asked.elapsed())
            });
            std::thread::sleep(checkout / 2);
            pg.stall_next_reset(reset * 30);
            pg.stall_next_connect(checkout * 5);
            drop(lease);
            waiter.join().expect("the waiter finishes")
        });
        assert!(
            matches!(leased, Err(crate::postgres::LeaseError::Connect(_))),
            "{leased:?}"
        );
        assert!(
            waited < checkout + Duration::from_millis(500),
            "the checkout ends when its wait runs out: {waited:?}"
        );
    }
}
