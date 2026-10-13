//! Idempotent operation and processed-result receipts.
//!
//! These records are not engine facts. They are host integration state: whether an operation
//! or processed result is claimed, pending, or complete. SQLite and Memory keep the rows in the
//! same database as the log; PostgreSQL hosts install the equivalent `openappa_*` tables
//! through their own migrations.
//!
//! A pending receipt carries a lease: the owner that claimed it and when that claim runs out.
//! The owner renews the lease while it drives the work, and releases it to end it at once.
//! Another owner takes a pending receipt over only once its lease has run out, and from then on
//! the owner it displaced can neither complete nor renew it. PostgreSQL judges expiry by the database clock; SQLite and Memory,
//! which one process owns, by the process clock.

use std::time::Duration;

use appa_engine::value::TrajectoryId;
use serde_json::Value;

/// The longest [`LeaseOwner`], in bytes.
const LEASE_OWNER_MAX_BYTES: usize = 256;

/// Who holds a pending receipt: one host process, or one unit of work within it. Never empty, at
/// most 256 bytes, and free of control characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LeaseOwner(String);

impl LeaseOwner {
    pub fn parse(text: &str) -> Result<Self, LeaseValueError> {
        if text.is_empty() {
            return Err(LeaseValueError::EmptyOwner);
        }
        if text.chars().any(char::is_control) {
            return Err(LeaseValueError::ControlCharacter);
        }
        match text.len() {
            bytes if bytes > LEASE_OWNER_MAX_BYTES => Err(LeaseValueError::OwnerLength { bytes }),
            _ => Ok(Self(text.to_owned())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LeaseOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The lease a claim takes or renews: its owner, and how long it stands without a renewal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptLease {
    owner: LeaseOwner,
    ttl: Duration,
}

impl ReceiptLease {
    pub fn new(owner: LeaseOwner, ttl: Duration) -> Result<Self, LeaseValueError> {
        if ttl.is_zero() {
            return Err(LeaseValueError::ZeroTtl);
        }
        Ok(Self { owner, ttl })
    }

    pub fn owner(&self) -> &LeaseOwner {
        &self.owner
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The TTL in whole microseconds, rounded up so a positive TTL never stores as none.
    #[cfg(feature = "postgres")]
    pub(crate) fn ttl_micros(&self) -> i64 {
        i64::try_from(self.ttl.as_nanos().div_ceil(1_000)).unwrap_or(i64::MAX)
    }

    /// The TTL in whole milliseconds, rounded up so a positive TTL never stores as none.
    pub(crate) fn ttl_millis(&self) -> i64 {
        i64::try_from(self.ttl.as_nanos().div_ceil(1_000_000)).unwrap_or(i64::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeaseValueError {
    #[error("a lease owner is empty")]
    EmptyOwner,
    #[error("a lease owner of {bytes} bytes; at most {LEASE_OWNER_MAX_BYTES}")]
    OwnerLength { bytes: usize },
    #[error("a lease owner carries a control character")]
    ControlCharacter,
    #[error("a lease TTL of zero")]
    ZeroTtl,
}

/// The organization and conversation session a receipt belongs to. Session ids are unique only
/// within an organization, so the organization is part of every receipt key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScope {
    pub organization_id: String,
    pub session_id: String,
}

/// Who besides the session may act on an operation receipt. A session-bound receipt records
/// its caller but never compares it; a caller-bound one admits only the caller that claimed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptBinding {
    Session { caller_id: Option<String> },
    Caller { caller_id: String },
}

impl ReceiptBinding {
    pub(crate) fn caller_id(&self) -> Option<&str> {
        match self {
            Self::Session { caller_id } => caller_id.as_deref(),
            Self::Caller { caller_id } => Some(caller_id),
        }
    }

    fn kind(&self) -> BindingKind {
        match self {
            Self::Session { .. } => BindingKind::Session,
            Self::Caller { .. } => BindingKind::Caller,
        }
    }

    fn admits(&self, requested: &ReceiptBinding) -> bool {
        match (self, requested) {
            (Self::Session { .. }, Self::Session { .. }) => true,
            (Self::Caller { caller_id: stored }, Self::Caller { caller_id: requested }) => stored == requested,
            (Self::Session { .. }, Self::Caller { .. }) | (Self::Caller { .. }, Self::Session { .. }) => false,
        }
    }
}

/// A binding as the stored input envelope spells it; the caller lives in its own column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum BindingKind {
    Session,
    Caller,
}

#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error("receipt storage failed: {0}")]
    Storage(String),
    #[error("receipt belongs to another authenticated scope")]
    ScopeMismatch,
    #[error("receipt key was reused with different input")]
    InputMismatch,
    #[error("receipt is absent or no longer pending")]
    NotPending,
    #[error("receipt was completed with a different value")]
    CompletionMismatch,
    #[error("receipt is pending under another lease owner")]
    LeaseLost,
}

/// The durable request key for an operation receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationKey {
    pub session: SessionScope,
    pub binding: ReceiptBinding,
    pub operation_id: String,
}

/// Immutable request recorded before executing work to ensure idempotent replay.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationRequest {
    pub key: OperationKey,
    pub root: TrajectoryId,
    /// The stable part used to detect a conflicting retry.
    pub input: Value,
    /// Optional non-semantic host context retained for a later result hook.
    /// Changes here never alter the idempotency contract.
    pub context: Option<Value>,
}

/// The result of claiming one operation receipt.
#[derive(Debug, Clone, PartialEq)]
pub enum OperationClaim {
    /// The claiming owner holds the receipt: it wrote it now, or held it already and the claim
    /// extended its lease. The caller runs the work.
    Claimed,
    /// The receipt was pending under a lease that had run out, and the claiming owner took it
    /// over. The work may have begun under the owner it displaced; the caller drives it again.
    Recovered,
    /// The receipt is pending under another owner's live lease. The caller must not run the work.
    Held,
    Complete {
        decision: Value,
    },
}

impl OperationClaim {
    /// Whether the claiming owner now holds the receipt, so the claim writes its lease.
    pub(crate) fn holds(&self) -> bool {
        match self {
            Self::Claimed | Self::Recovered => true,
            Self::Held | Self::Complete { .. } => false,
        }
    }
}

/// The durable key for a processed tool result receipt. It is always session-bound: the
/// caller is recorded but never compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedResultKey {
    pub session: SessionScope,
    pub caller_id: Option<String>,
    pub tool_call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedResultRequest {
    pub key: ProcessedResultKey,
    pub root: TrajectoryId,
}

/// The result of claiming one processed-result receipt; the variants mean what
/// [`OperationClaim`]'s do.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessedResultClaim {
    Claimed,
    Recovered,
    Held,
    Complete { approved_output: String, decision: Value },
}

impl ProcessedResultClaim {
    /// Whether the claiming owner now holds the receipt, so the claim writes its lease.
    pub(crate) fn holds(&self) -> bool {
        match self {
            Self::Claimed | Self::Recovered => true,
            Self::Held | Self::Complete { .. } => false,
        }
    }
}

/// What renewing an owner's receipts found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    /// Every listed receipt is still pending under the caller's lease, now extended, or the
    /// caller completed it.
    Kept,
    /// Some listed receipt is gone, or another owner took it over, pending or complete. The
    /// work it guarded is no longer the caller's to finish.
    Lost,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredOperationInput {
    version: u8,
    binding: BindingKind,
    pub(crate) semantic: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<Value>,
}

impl StoredOperationInput {
    pub(crate) fn from_request(request: &OperationRequest) -> Self {
        Self {
            version: 1,
            binding: request.key.binding.kind(),
            semantic: request.input.clone(),
            context: request.context.clone(),
        }
    }

    pub(crate) fn decode(value: Value) -> Result<Self, ReceiptError> {
        match serde_json::from_value::<Self>(value) {
            Ok(stored) if stored.version == 1 => Ok(stored),
            Ok(stored) => Err(ReceiptError::storage(format!(
                "operation receipt has unsupported version {}",
                stored.version
            ))),
            Err(error) => Err(ReceiptError::storage(format!(
                "operation receipt input is not a v1 envelope: {error}"
            ))),
        }
    }

    /// The binding this envelope names, with the caller its row records.
    pub(crate) fn binding(&self, caller_id: Option<String>) -> Result<ReceiptBinding, ReceiptError> {
        match (self.binding, caller_id) {
            (BindingKind::Session, caller_id) => Ok(ReceiptBinding::Session { caller_id }),
            (BindingKind::Caller, Some(caller_id)) => Ok(ReceiptBinding::Caller { caller_id }),
            (BindingKind::Caller, None) => Err(ReceiptError::storage("caller-bound operation receipt lacks a caller")),
        }
    }
}

/// A stored JSON column, decoded when read but refused only where a check reaches it.
pub(crate) type StoredJson = Result<Value, ReceiptError>;

/// A stored receipt's lease, its expiry judged by the backend's clock when the row was read.
pub(crate) struct StoredLease {
    pub(crate) owner: Option<String>,
    /// The lease had not run out. A row without an expiry has none left.
    pub(crate) live: bool,
}

impl StoredLease {
    fn held_by(&self, owner: &LeaseOwner) -> bool {
        self.owner.as_deref() == Some(owner.as_str())
    }

    /// Who drives a pending receipt that `owner` claims.
    fn standing(&self, owner: &LeaseOwner) -> Standing {
        match (self.held_by(owner), self.live) {
            (true, _) => Standing::Mine,
            (false, true) => Standing::Others,
            (false, false) => Standing::Lapsed,
        }
    }
}

/// A pending receipt as one claiming owner finds it.
enum Standing {
    /// The owner already holds it, whether or not its lease has run out: nobody displaced it.
    Mine,
    /// Another owner holds it under a live lease.
    Others,
    /// Another owner's lease, or no lease, has run out.
    Lapsed,
}

/// One operation receipt row, its input envelope decoded.
pub(crate) struct StoredOperation {
    pub(crate) session: SessionScope,
    pub(crate) binding: ReceiptBinding,
    pub(crate) root: String,
    pub(crate) semantic: Value,
    pub(crate) status: String,
    pub(crate) decision: Option<StoredJson>,
    pub(crate) lease: StoredLease,
}

impl StoredOperation {
    /// Whether `requested` may act on this receipt: the same session and binding, and for a
    /// caller-bound receipt the same caller.
    fn owned_by(&self, requested: &OperationKey) -> bool {
        self.session == requested.session && self.binding.admits(&requested.binding)
    }
}

/// One processed-result receipt row.
pub(crate) struct StoredResult {
    pub(crate) session: SessionScope,
    pub(crate) root: String,
    pub(crate) status: String,
    pub(crate) approved_output: Option<String>,
    pub(crate) decision: Option<StoredJson>,
    pub(crate) lease: StoredLease,
}

/// What a completion does to the row it found.
pub(crate) enum Completion {
    /// The receipt was pending under the completing owner: record the completion.
    Write,
    /// The receipt already holds this very completion.
    Unchanged,
}

/// A claim on a receipt row that already exists; a claim that finds none writes it and is
/// [`OperationClaim::Claimed`]. A completed receipt replays its decision, and a pending one
/// resolves by its lease. [`OperationClaim::holds`] tells the backend to write the claiming
/// owner's lease.
pub(crate) fn resolve_operation_claim(
    stored: StoredOperation,
    request: &OperationRequest,
    owner: &LeaseOwner,
) -> Result<OperationClaim, ReceiptError> {
    if !stored.owned_by(&request.key) || stored.root != request.root.as_str() {
        return Err(ReceiptError::ScopeMismatch);
    }
    if stored.semantic != request.input {
        return Err(ReceiptError::InputMismatch);
    }
    match stored.status.as_str() {
        "complete" => Ok(OperationClaim::Complete {
            decision: stored
                .decision
                .ok_or_else(|| ReceiptError::storage("complete operation lacks a decision"))??,
        }),
        "pending" => Ok(match stored.lease.standing(owner) {
            Standing::Mine => OperationClaim::Claimed,
            Standing::Others => OperationClaim::Held,
            Standing::Lapsed => OperationClaim::Recovered,
        }),
        _ => Err(ReceiptError::storage("operation receipt has an invalid status")),
    }
}

/// A pending receipt completes only under the owner whose lease it carries; a completed one
/// accepts its own completion again from anyone.
pub(crate) fn resolve_operation_completion(
    existing: Option<StoredOperation>,
    key: &OperationKey,
    decision: &Value,
    owner: &LeaseOwner,
) -> Result<Completion, ReceiptError> {
    let Some(stored) = existing else {
        return Err(ReceiptError::NotPending);
    };
    if !stored.owned_by(key) {
        return Err(ReceiptError::ScopeMismatch);
    }
    match stored.status.as_str() {
        "pending" if stored.lease.held_by(owner) => Ok(Completion::Write),
        "pending" => Err(ReceiptError::LeaseLost),
        "complete" => match stored.decision.transpose()?.as_ref() == Some(decision) {
            true => Ok(Completion::Unchanged),
            false => Err(ReceiptError::CompletionMismatch),
        },
        _ => Err(ReceiptError::storage("operation receipt has an invalid status")),
    }
}

/// A claim on a result row that already exists, as [`resolve_operation_claim`] is.
pub(crate) fn resolve_result_claim(
    stored: StoredResult,
    request: &ProcessedResultRequest,
    owner: &LeaseOwner,
) -> Result<ProcessedResultClaim, ReceiptError> {
    if stored.session != request.key.session || stored.root != request.root.as_str() {
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
        "pending" => Ok(match stored.lease.standing(owner) {
            Standing::Mine => ProcessedResultClaim::Claimed,
            Standing::Others => ProcessedResultClaim::Held,
            Standing::Lapsed => ProcessedResultClaim::Recovered,
        }),
        _ => Err(ReceiptError::storage("processed result has an invalid status")),
    }
}

/// A result completes as [`resolve_operation_completion`] lets an operation complete.
pub(crate) fn resolve_result_completion(
    existing: Option<StoredResult>,
    key: &ProcessedResultKey,
    approved_output: &str,
    decision: &Value,
    owner: &LeaseOwner,
) -> Result<Completion, ReceiptError> {
    let Some(stored) = existing else {
        return Err(ReceiptError::NotPending);
    };
    if stored.session != key.session {
        return Err(ReceiptError::ScopeMismatch);
    }
    match stored.status.as_str() {
        "pending" if stored.lease.held_by(owner) => Ok(Completion::Write),
        "pending" => Err(ReceiptError::LeaseLost),
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
        Self::Storage(detail.into())
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::held::millis;
    use crate::{Backend, LogStore};

    const TTL: Duration = Duration::from_secs(60);

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

    fn session() -> SessionScope {
        SessionScope {
            organization_id: "org".to_owned(),
            session_id: "session".to_owned(),
        }
    }

    fn caller_bound(caller: &str) -> ReceiptBinding {
        ReceiptBinding::Caller {
            caller_id: caller.to_owned(),
        }
    }

    fn session_bound(caller: &str) -> ReceiptBinding {
        ReceiptBinding::Session {
            caller_id: Some(caller.to_owned()),
        }
    }

    fn operation(binding: ReceiptBinding) -> OperationRequest {
        OperationRequest {
            key: OperationKey {
                session: session(),
                binding,
                operation_id: "op-1".to_owned(),
            },
            root: TrajectoryId::new("root"),
            input: serde_json::json!({"offer_id": "0123456789abcdef"}),
            context: None,
        }
    }

    fn result(caller: &str) -> ProcessedResultRequest {
        ProcessedResultRequest {
            key: ProcessedResultKey {
                session: session(),
                caller_id: Some(caller.to_owned()),
                tool_call_id: "call-1".to_owned(),
            },
            root: TrajectoryId::new("root"),
        }
    }

    fn owner(name: &str) -> LeaseOwner {
        LeaseOwner::parse(name).expect("the owner is well formed")
    }

    fn lease(name: &str) -> ReceiptLease {
        ReceiptLease::new(owner(name), TTL).expect("the lease is well formed")
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

    /// The owner and expiry of the one receipt row in `table`.
    fn lease_of(store: &LogStore, table: &str) -> (Option<String>, Option<i64>) {
        store
            .lock()
            .query_row(
                &format!("SELECT lease_owner, lease_expires_at FROM {table}"),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the lease reads")
    }

    fn execute(store: &LogStore, sql: &str) {
        store.lock().execute(sql, []).expect("the fixture statement runs");
    }

    /// Run every lease out without waiting.
    fn lapse(store: &LogStore) {
        execute(store, "UPDATE operations SET lease_expires_at = 0");
        execute(store, "UPDATE processed_results SET lease_expires_at = 0");
    }

    fn stored_operation(status: &str, holder: Option<&str>, live: bool) -> StoredOperation {
        StoredOperation {
            session: session(),
            binding: caller_bound("caller"),
            root: "root".to_owned(),
            semantic: serde_json::json!({"offer_id": "0123456789abcdef"}),
            status: status.to_owned(),
            decision: (status == "complete").then(|| Ok(serde_json::json!({"decision": "allow_call"}))),
            lease: StoredLease {
                owner: holder.map(str::to_owned),
                live,
            },
        }
    }

    fn stored_result(status: &str, holder: Option<&str>, live: bool) -> StoredResult {
        StoredResult {
            session: session(),
            root: "root".to_owned(),
            status: status.to_owned(),
            approved_output: (status == "complete").then(|| "approved".to_owned()),
            decision: (status == "complete").then(|| Ok(serde_json::json!({"decision": "allow"}))),
            lease: StoredLease {
                owner: holder.map(str::to_owned),
                live,
            },
        }
    }

    /// Every lease a stored row can carry: held by the claimer or by another owner, or by
    /// nobody, each live or run out.
    const LEASES: [(Option<&str>, bool); 6] = [
        (Some("a"), true),
        (Some("a"), false),
        (Some("b"), true),
        (Some("b"), false),
        (None, true),
        (None, false),
    ];

    /// A pending receipt resolves by its lease alone: the claimer's own, whatever its expiry,
    /// is claimed again; another owner's live lease holds the claim off; a lease that ran out,
    /// or none, is recovered.
    #[test]
    fn a_pending_claim_resolves_by_its_lease() {
        use OperationClaim as Op;
        use ProcessedResultClaim as Res;
        let expected = [
            (Op::Claimed, Res::Claimed),
            (Op::Claimed, Res::Claimed),
            (Op::Held, Res::Held),
            (Op::Recovered, Res::Recovered),
            (Op::Held, Res::Held),
            (Op::Recovered, Res::Recovered),
        ];
        for ((holder, live), (operation_claim, result_claim)) in LEASES.into_iter().zip(expected) {
            assert_eq!(
                resolve_operation_claim(
                    stored_operation("pending", holder, live),
                    &operation(caller_bound("caller")),
                    &owner("a")
                )
                .expect("the operation resolves"),
                operation_claim,
                "{holder:?} live={live}"
            );
            assert_eq!(
                resolve_result_claim(stored_result("pending", holder, live), &result("caller"), &owner("a"))
                    .expect("the result resolves"),
                result_claim,
                "{holder:?} live={live}"
            );
            assert_eq!(
                operation_claim.holds(),
                matches!(operation_claim, Op::Claimed | Op::Recovered)
            );
            assert_eq!(
                result_claim.holds(),
                matches!(result_claim, Res::Claimed | Res::Recovered)
            );
        }
    }

    #[test]
    fn a_completed_claim_replays_under_any_lease() {
        for (holder, live) in LEASES {
            assert_eq!(
                resolve_operation_claim(
                    stored_operation("complete", holder, live),
                    &operation(caller_bound("caller")),
                    &owner("a")
                )
                .expect("the operation replays"),
                OperationClaim::Complete {
                    decision: serde_json::json!({"decision": "allow_call"})
                }
            );
            assert_eq!(
                resolve_result_claim(stored_result("complete", holder, live), &result("caller"), &owner("a"))
                    .expect("the result replays"),
                ProcessedResultClaim::Complete {
                    approved_output: "approved".to_owned(),
                    decision: serde_json::json!({"decision": "allow"}),
                }
            );
        }
    }

    /// The scope and input checks come before the lease, so a mismatch refuses even a claim
    /// the lease would admit.
    #[test]
    fn a_mismatched_claim_refuses_whatever_the_lease() {
        let mut other_input = operation(caller_bound("caller"));
        other_input.input = serde_json::json!({"offer_id": "other"});
        let mut other_root = result("caller");
        other_root.root = TrajectoryId::new("other-root");
        for (holder, live) in LEASES {
            for status in ["pending", "complete"] {
                assert!(matches!(
                    resolve_operation_claim(
                        stored_operation(status, holder, live),
                        &operation(caller_bound("other")),
                        &owner("a")
                    ),
                    Err(ReceiptError::ScopeMismatch)
                ));
                assert!(matches!(
                    resolve_operation_claim(stored_operation(status, holder, live), &other_input, &owner("a")),
                    Err(ReceiptError::InputMismatch)
                ));
                assert!(matches!(
                    resolve_result_claim(stored_result(status, holder, live), &other_root, &owner("a")),
                    Err(ReceiptError::ScopeMismatch)
                ));
            }
        }
    }

    /// A pending receipt completes under the owner its lease names, even once that lease has
    /// run out, and under nobody else. A completed one accepts its own completion from anyone.
    #[test]
    fn completion_is_fenced_by_the_lease_owner() {
        let key = operation(caller_bound("caller")).key;
        let (allow, deny) = (
            serde_json::json!({"decision": "allow_call"}),
            serde_json::json!({"decision": "deny"}),
        );
        let result_key = result("caller").key;
        let decided = serde_json::json!({"decision": "allow"});
        for (holder, live) in LEASES {
            let mine = holder == Some("a");
            let operation_completion = resolve_operation_completion(
                Some(stored_operation("pending", holder, live)),
                &key,
                &allow,
                &owner("a"),
            );
            let result_completion = resolve_result_completion(
                Some(stored_result("pending", holder, live)),
                &result_key,
                "approved",
                &decided,
                &owner("a"),
            );
            for completion in [operation_completion, result_completion] {
                match mine {
                    true => assert!(matches!(completion, Ok(Completion::Write)), "{holder:?} live={live}"),
                    false => assert!(
                        matches!(completion, Err(ReceiptError::LeaseLost)),
                        "{holder:?} live={live}"
                    ),
                }
            }
            for completer in ["a", "b"] {
                let completer = owner(completer);
                let complete = || Some(stored_operation("complete", holder, live));
                assert!(matches!(
                    resolve_operation_completion(complete(), &key, &allow, &completer),
                    Ok(Completion::Unchanged)
                ));
                assert!(matches!(
                    resolve_operation_completion(complete(), &key, &deny, &completer),
                    Err(ReceiptError::CompletionMismatch)
                ));
                let complete = || Some(stored_result("complete", holder, live));
                assert!(matches!(
                    resolve_result_completion(complete(), &result_key, "approved", &decided, &completer),
                    Ok(Completion::Unchanged)
                ));
                assert!(matches!(
                    resolve_result_completion(complete(), &result_key, "changed", &decided, &completer),
                    Err(ReceiptError::CompletionMismatch)
                ));
            }
        }
        assert!(matches!(
            resolve_operation_completion(None, &key, &allow, &owner("a")),
            Err(ReceiptError::NotPending)
        ));
        assert!(matches!(
            resolve_operation_completion(
                Some(stored_operation("pending", Some("a"), true)),
                &operation(caller_bound("other")).key,
                &allow,
                &owner("a")
            ),
            Err(ReceiptError::ScopeMismatch)
        ));
    }

    #[test]
    fn lease_values_are_validated() {
        assert_eq!(LeaseOwner::parse(""), Err(LeaseValueError::EmptyOwner));
        assert_eq!(LeaseOwner::parse("pod\n1"), Err(LeaseValueError::ControlCharacter));
        assert_eq!(
            LeaseOwner::parse(&"x".repeat(257)),
            Err(LeaseValueError::OwnerLength { bytes: 257 })
        );
        assert_eq!(
            LeaseOwner::parse(&"x".repeat(256)).map(|owner| owner.as_str().len()),
            Ok(256)
        );
        assert_eq!(
            ReceiptLease::new(owner("a"), Duration::ZERO),
            Err(LeaseValueError::ZeroTtl)
        );
        let brief = ReceiptLease::new(owner("a"), Duration::from_nanos(1)).expect("a positive TTL is a lease");
        assert_eq!(brief.ttl_millis(), 1, "a positive TTL never stores as none");
        assert_eq!(lease("a").ttl_millis(), 60_000);
    }

    #[test]
    fn operation_receipts_store_the_frozen_v1_input_envelope() {
        let envelope =
            r#"{"version":1,"binding":"caller","semantic":{"offer_id":"0123456789abcdef"},"context":{"hook":"pre"}}"#;
        for (store, _dir) in stores() {
            let mut request = operation(caller_bound("caller"));
            request.context = Some(serde_json::json!({"hook": "pre"}));
            store
                .claim_operation(request.clone(), &lease("a"))
                .expect("the operation claims");
            assert_eq!(operation_row(&store), (envelope.to_owned(), "pending".to_owned(), None));
            store
                .complete_operation(
                    request.key.clone(),
                    serde_json::json!({"decision": "allow_call"}),
                    &owner("a"),
                )
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
                .claim_operation(operation(session_bound("caller")), &lease("a"))
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
                let request = operation(session_bound("caller"));
                assert!(
                    matches!(
                        store.claim_operation(request.clone(), &lease("a")),
                        Err(ReceiptError::Storage(_))
                    ),
                    "{input}"
                );
                assert!(
                    matches!(
                        store.complete_operation(
                            request.key,
                            serde_json::json!({"decision": "allow_call"}),
                            &owner("a")
                        ),
                        Err(ReceiptError::Storage(_))
                    ),
                    "{input}"
                );
            }
        }
    }

    #[test]
    fn a_caller_bound_operation_row_without_a_caller_is_a_storage_failure() {
        for (store, _dir) in stores() {
            store
                .lock()
                .execute(
                    "INSERT INTO operations (organization_id, caller_id, session_id, operation_id, root, input, status)
                     VALUES ('org', NULL, 'session', 'op-1', 'root', ?1, 'pending')",
                    [r#"{"version":1,"binding":"caller","semantic":{"offer_id":"0123456789abcdef"}}"#],
                )
                .expect("the fixture row inserts");
            let request = operation(caller_bound("caller"));
            assert!(matches!(
                store.claim_operation(request.clone(), &lease("a")),
                Err(ReceiptError::Storage(_))
            ));
            assert!(matches!(
                store.complete_operation(request.key, serde_json::json!({"decision": "allow_call"}), &owner("a")),
                Err(ReceiptError::Storage(_))
            ));
        }
    }

    #[test]
    fn operation_receipts_refuse_another_scope_without_writing() {
        for (store, _dir) in stores() {
            let owned = operation(caller_bound("caller"));
            store
                .claim_operation(owned.clone(), &lease("a"))
                .expect("the operation claims");

            let other_caller = operation(caller_bound("other"));
            let session_bound = operation(session_bound("caller"));
            let mut other_root = owned.clone();
            other_root.root = TrajectoryId::new("other-root");
            for request in [&other_caller, &session_bound, &other_root] {
                assert!(matches!(
                    store.claim_operation(request.clone(), &lease("a")),
                    Err(ReceiptError::ScopeMismatch)
                ));
            }
            for key in [&other_caller.key, &session_bound.key] {
                assert!(matches!(
                    store.complete_operation(key.clone(), serde_json::json!({"decision": "deny"}), &owner("a")),
                    Err(ReceiptError::ScopeMismatch)
                ));
            }
            assert_eq!(operation_row(&store).1, "pending");
            store
                .complete_operation(
                    owned.key.clone(),
                    serde_json::json!({"decision": "allow_call"}),
                    &owner("a"),
                )
                .expect("the owning scope completes");
        }
    }

    /// A session-bound receipt does not compare caller identities; its lease still decides
    /// who drives it.
    #[test]
    fn a_session_bound_operation_ignores_the_caller() {
        for (store, _dir) in stores() {
            store
                .claim_operation(operation(session_bound("caller")), &lease("a"))
                .expect("the operation claims");
            assert_eq!(
                store
                    .claim_operation(operation(session_bound("other")), &lease("b"))
                    .expect("the claim resolves"),
                OperationClaim::Held
            );
            assert_eq!(
                store
                    .claim_operation(operation(session_bound("other")), &lease("a"))
                    .expect("the claim resolves"),
                OperationClaim::Claimed
            );
            store
                .complete_operation(
                    operation(session_bound("other")).key,
                    serde_json::json!({"decision": "allow_call"}),
                    &owner("a"),
                )
                .expect("another caller in the session completes");
        }
    }

    /// Each receipt row records the caller of the claim that wrote it, or none, and a later
    /// completion by another caller of the session leaves it as claimed.
    #[test]
    fn receipts_record_the_caller_that_claimed_them() {
        let caller_of = |store: &LogStore, table: &str| -> Vec<Option<String>> {
            let connection = store.lock();
            let mut statement = connection
                .prepare(&format!("SELECT caller_id FROM {table} ORDER BY rowid"))
                .expect("the caller query prepares");
            statement
                .query_map([], |row| row.get(0))
                .expect("the callers read")
                .collect::<Result<_, _>>()
                .expect("every caller reads")
        };
        for (store, _dir) in stores() {
            for (operation_id, binding) in [
                ("op-caller", caller_bound("caller")),
                ("op-session", session_bound("session-caller")),
                ("op-callerless", ReceiptBinding::Session { caller_id: None }),
            ] {
                let mut request = operation(binding);
                request.key.operation_id = operation_id.to_owned();
                store
                    .claim_operation(request, &lease("a"))
                    .expect("the operation claims");
            }
            let mut completed_elsewhere = operation(session_bound("other"));
            completed_elsewhere.key.operation_id = "op-session".to_owned();
            store
                .complete_operation(
                    completed_elsewhere.key,
                    serde_json::json!({"decision": "allow_call"}),
                    &owner("a"),
                )
                .expect("another caller in the session completes");
            assert_eq!(
                caller_of(&store, "operations"),
                [Some("caller".to_owned()), Some("session-caller".to_owned()), None]
            );

            let mut callerless_result = result("caller");
            callerless_result.key.caller_id = None;
            callerless_result.key.tool_call_id = "call-callerless".to_owned();
            for request in [result("caller"), callerless_result] {
                store
                    .claim_processed_result(request, &lease("a"))
                    .expect("the result claims");
            }
            store
                .complete_processed_result(
                    result("other").key,
                    "approved".to_owned(),
                    serde_json::json!({"decision": "allow"}),
                    &owner("a"),
                )
                .expect("another caller in the session completes");
            assert_eq!(
                caller_of(&store, "processed_results"),
                [Some("caller".to_owned()), None]
            );
        }
    }

    #[test]
    fn pending_receipts_are_found_by_their_root_alone() {
        for (store, _dir) in stores() {
            let pending = |root: &str| {
                store
                    .has_pending_receipts(&TrajectoryId::new(root))
                    .expect("the check reads")
            };
            assert!(!pending("root"));

            let request = operation(session_bound("caller"));
            store
                .claim_operation(request.clone(), &lease("a"))
                .expect("the operation claims");
            assert!(pending("root"), "a pending operation");
            assert!(!pending("other-root"));
            store
                .complete_operation(request.key, serde_json::json!({"decision": "allow_call"}), &owner("a"))
                .expect("the operation completes");
            assert!(!pending("root"));

            let request = result("caller");
            store
                .claim_processed_result(request.clone(), &lease("a"))
                .expect("the result claims");
            assert!(pending("root"), "a pending processed result");
            assert!(!pending("other-root"));
            store
                .complete_processed_result(
                    request.key,
                    "approved".to_owned(),
                    serde_json::json!({"decision": "allow"}),
                    &owner("a"),
                )
                .expect("the result completes");
            assert!(!pending("root"));
        }
    }

    #[test]
    fn processed_results_refuse_another_scope_without_writing() {
        for (store, _dir) in stores() {
            let owned = result("caller");
            store
                .claim_processed_result(owned.clone(), &lease("a"))
                .expect("the result claims");

            let mut other_root = owned.clone();
            other_root.root = TrajectoryId::new("other-root");
            assert!(matches!(
                store.claim_processed_result(other_root, &lease("a")),
                Err(ReceiptError::ScopeMismatch)
            ));
            assert_eq!(
                store
                    .claim_processed_result(result("other"), &lease("b"))
                    .expect("the claim resolves"),
                ProcessedResultClaim::Held
            );
            store
                .complete_processed_result(
                    owned.key.clone(),
                    "approved".to_owned(),
                    serde_json::json!({"decision": "allow"}),
                    &owner("a"),
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
                    .claim_processed_result(request.clone(), &lease("a"))
                    .expect("the result claims"),
                ProcessedResultClaim::Claimed
            );
            store
                .complete_processed_result(
                    request.key.clone(),
                    "approved".to_owned(),
                    decision.clone(),
                    &owner("a"),
                )
                .expect("the result completes");
            assert_eq!(
                store
                    .claim_processed_result(request, &lease("b"))
                    .expect("the completed result replays"),
                ProcessedResultClaim::Complete {
                    approved_output: "approved".to_owned(),
                    decision,
                }
            );
        }
    }

    #[test]
    fn a_pending_receipt_holds_off_another_owner_and_refuses_other_input() {
        for (store, _dir) in stores() {
            let request = operation(caller_bound("caller"));
            store
                .claim_operation(request.clone(), &lease("a"))
                .expect("the operation claims");
            let mut changed = request.clone();
            changed.input = serde_json::json!({"offer_id": "other"});
            assert!(matches!(
                store.claim_operation(changed, &lease("a")),
                Err(ReceiptError::InputMismatch)
            ));
            let mut recontextualized = request.clone();
            recontextualized.context = Some(serde_json::json!({"hook": "post"}));
            assert_eq!(
                store
                    .claim_operation(recontextualized, &lease("b"))
                    .expect("the claim resolves"),
                OperationClaim::Held
            );
            assert_eq!(
                store.claim_operation(request, &lease("b")).expect("the claim resolves"),
                OperationClaim::Held
            );
            assert_eq!(
                lease_of(&store, "operations").0.as_deref(),
                Some("a"),
                "a held claim writes nothing"
            );

            let request = result("caller");
            store
                .claim_processed_result(request.clone(), &lease("a"))
                .expect("the result claims");
            assert_eq!(
                store
                    .claim_processed_result(request, &lease("b"))
                    .expect("the claim resolves"),
                ProcessedResultClaim::Held
            );
        }
    }

    #[test]
    fn completing_an_absent_receipt_is_not_pending() {
        for (store, _dir) in stores() {
            assert!(matches!(
                store.complete_operation(
                    operation(session_bound("caller")).key,
                    serde_json::json!({}),
                    &owner("a")
                ),
                Err(ReceiptError::NotPending)
            ));
            assert!(matches!(
                store.complete_processed_result(
                    result("caller").key,
                    "approved".to_owned(),
                    serde_json::json!({}),
                    &owner("a")
                ),
                Err(ReceiptError::NotPending)
            ));
        }
    }

    #[test]
    fn a_completed_receipt_accepts_only_its_own_completion_again() {
        for (store, _dir) in stores() {
            let decision = serde_json::json!({"decision": "allow_call"});
            let other = serde_json::json!({"decision": "deny"});

            let request = operation(caller_bound("caller"));
            store
                .claim_operation(request.clone(), &lease("a"))
                .expect("the operation claims");
            store
                .complete_operation(request.key.clone(), decision.clone(), &owner("a"))
                .expect("the operation completes");
            for completer in ["a", "b"] {
                store
                    .complete_operation(request.key.clone(), decision.clone(), &owner(completer))
                    .expect("the same completion is idempotent for anyone");
                assert!(matches!(
                    store.complete_operation(request.key.clone(), other.clone(), &owner(completer)),
                    Err(ReceiptError::CompletionMismatch)
                ));
            }

            let request = result("caller");
            store
                .claim_processed_result(request.clone(), &lease("a"))
                .expect("the result claims");
            store
                .complete_processed_result(
                    request.key.clone(),
                    "approved".to_owned(),
                    decision.clone(),
                    &owner("a"),
                )
                .expect("the result completes");
            for completer in ["a", "b"] {
                store
                    .complete_processed_result(
                        request.key.clone(),
                        "approved".to_owned(),
                        decision.clone(),
                        &owner(completer),
                    )
                    .expect("the same completion is idempotent for anyone");
                assert!(matches!(
                    store.complete_processed_result(
                        request.key.clone(),
                        "changed".to_owned(),
                        decision.clone(),
                        &owner(completer)
                    ),
                    Err(ReceiptError::CompletionMismatch)
                ));
                assert!(matches!(
                    store.complete_processed_result(
                        request.key.clone(),
                        "approved".to_owned(),
                        other.clone(),
                        &owner(completer)
                    ),
                    Err(ReceiptError::CompletionMismatch)
                ));
            }
        }
    }

    /// Another owner's claim is held off while the lease is live and recovers the receipt
    /// once it has run out. The owner it displaced can then neither complete nor renew it.
    #[test]
    fn a_lapsed_lease_passes_to_the_next_owner_and_fences_out_the_first() {
        for (store, _dir) in stores() {
            let request = operation(caller_bound("caller"));
            let result_request = result("caller");
            let decision = serde_json::json!({"decision": "allow_call"});
            assert_eq!(
                store.claim_operation(request.clone(), &lease("a")).unwrap(),
                OperationClaim::Claimed
            );
            assert_eq!(
                store
                    .claim_processed_result(result_request.clone(), &lease("a"))
                    .unwrap(),
                ProcessedResultClaim::Claimed
            );
            assert_eq!(
                store.claim_operation(request.clone(), &lease("b")).unwrap(),
                OperationClaim::Held
            );

            lapse(&store);
            assert_eq!(
                store.claim_operation(request.clone(), &lease("b")).unwrap(),
                OperationClaim::Recovered
            );
            assert_eq!(
                store
                    .claim_processed_result(result_request.clone(), &lease("b"))
                    .unwrap(),
                ProcessedResultClaim::Recovered
            );
            for table in ["operations", "processed_results"] {
                let (holder, expires_at) = lease_of(&store, table);
                assert_eq!(holder.as_deref(), Some("b"), "{table}");
                assert!(
                    expires_at.is_some_and(|at| at > millis(SystemTime::now())),
                    "a recovery starts a fresh lease"
                );
            }
            assert_eq!(
                store.claim_operation(request.clone(), &lease("a")).unwrap(),
                OperationClaim::Held,
                "the displaced owner is held off in turn"
            );

            assert!(matches!(
                store.complete_operation(request.key.clone(), decision.clone(), &owner("a")),
                Err(ReceiptError::LeaseLost)
            ));
            assert!(matches!(
                store.complete_processed_result(
                    result_request.key.clone(),
                    "approved".to_owned(),
                    decision.clone(),
                    &owner("a")
                ),
                Err(ReceiptError::LeaseLost)
            ));
            assert_eq!(
                store
                    .renew_receipts(&lease("a"), std::slice::from_ref(&request.key), &[])
                    .unwrap(),
                Renewal::Lost
            );
            assert_eq!(
                store
                    .renew_receipts(&lease("a"), &[], std::slice::from_ref(&result_request.key))
                    .unwrap(),
                Renewal::Lost
            );
            assert_eq!(operation_row(&store).1, "pending");

            store
                .complete_operation(request.key.clone(), decision.clone(), &owner("b"))
                .expect("the new owner completes");
            store
                .complete_processed_result(
                    result_request.key.clone(),
                    "approved".to_owned(),
                    decision.clone(),
                    &owner("b"),
                )
                .expect("the new owner completes");
            store
                .complete_operation(request.key.clone(), decision, &owner("a"))
                .expect("a completed receipt accepts its own completion from anyone");
            assert_eq!(
                store
                    .renew_receipts(
                        &lease("a"),
                        std::slice::from_ref(&request.key),
                        std::slice::from_ref(&result_request.key)
                    )
                    .unwrap(),
                Renewal::Lost,
                "a receipt another owner took over and completed stays lost to the displaced one"
            );
            assert_eq!(
                store
                    .renew_receipts(&lease("b"), &[request.key], &[result_request.key])
                    .unwrap(),
                Renewal::Kept,
                "a receipt its owner completed is kept"
            );
        }
    }

    /// A claim by the owner already holding the receipt extends its lease, even one that ran
    /// out, since nobody displaced it, and that owner still completes after its lease ran out.
    #[test]
    fn the_holding_owner_claims_again_and_completes_after_its_lease_lapsed() {
        for (store, _dir) in stores() {
            let request = operation(session_bound("caller"));
            let result_request = result("caller");
            store.claim_operation(request.clone(), &lease("a")).unwrap();
            store
                .claim_processed_result(result_request.clone(), &lease("a"))
                .unwrap();
            lapse(&store);
            assert_eq!(
                store.claim_operation(request.clone(), &lease("a")).unwrap(),
                OperationClaim::Claimed
            );
            assert_eq!(
                store
                    .claim_processed_result(result_request.clone(), &lease("a"))
                    .unwrap(),
                ProcessedResultClaim::Claimed
            );
            for table in ["operations", "processed_results"] {
                assert!(
                    lease_of(&store, table)
                        .1
                        .is_some_and(|at| at > millis(SystemTime::now())),
                    "the claim extends the lease in {table}"
                );
            }

            lapse(&store);
            store
                .complete_operation(request.key, serde_json::json!({"decision": "allow_call"}), &owner("a"))
                .expect("an owner nobody displaced completes");
            store
                .complete_processed_result(
                    result_request.key,
                    "approved".to_owned(),
                    serde_json::json!({"decision": "allow"}),
                    &owner("a"),
                )
                .expect("an owner nobody displaced completes");
        }
    }

    /// A renewal extends the caller's pending receipts, even ones whose lease ran out, and
    /// reports a listed receipt that no longer exists.
    #[test]
    fn a_renewal_extends_the_owners_receipts_and_reports_a_missing_one() {
        for (store, _dir) in stores() {
            let request = operation(caller_bound("caller"));
            let result_request = result("caller");
            store.claim_operation(request.clone(), &lease("a")).unwrap();
            store
                .claim_processed_result(result_request.clone(), &lease("a"))
                .unwrap();
            lapse(&store);
            assert_eq!(
                store
                    .renew_receipts(
                        &lease("a"),
                        std::slice::from_ref(&request.key),
                        std::slice::from_ref(&result_request.key)
                    )
                    .unwrap(),
                Renewal::Kept
            );
            for table in ["operations", "processed_results"] {
                assert!(
                    lease_of(&store, table)
                        .1
                        .is_some_and(|at| at > millis(SystemTime::now())),
                    "the renewal extends the lease in {table}"
                );
            }
            let mut missing = request.key.clone();
            missing.operation_id = "op-missing".to_owned();
            let mut missing_result = result_request.key.clone();
            missing_result.tool_call_id = "call-missing".to_owned();
            assert_eq!(
                store
                    .renew_receipts(&lease("a"), &[request.key.clone(), missing], &[])
                    .unwrap(),
                Renewal::Lost
            );
            assert_eq!(
                store.renew_receipts(&lease("a"), &[], &[missing_result]).unwrap(),
                Renewal::Lost
            );
            assert_eq!(store.renew_receipts(&lease("a"), &[], &[]).unwrap(), Renewal::Kept);
        }
    }

    /// A release ends the caller's own leases and keeps the rows, with the input they
    /// recorded, so the next claim recovers them at once. Completed receipts and other owners'
    /// leases are untouched.
    #[test]
    fn a_release_lets_the_next_claim_recover_at_once() {
        for (store, _dir) in stores() {
            let keyed = |id: &str| {
                let mut request = operation(caller_bound("caller"));
                request.key.operation_id = id.to_owned();
                request.context = Some(serde_json::json!({"hook": "pre"}));
                request
            };
            let (mine, theirs, done) = (keyed("op-mine"), keyed("op-theirs"), keyed("op-done"));
            store.claim_operation(mine.clone(), &lease("a")).unwrap();
            store.claim_operation(theirs.clone(), &lease("b")).unwrap();
            store.claim_operation(done.clone(), &lease("a")).unwrap();
            let decision = serde_json::json!({"decision": "allow_call"});
            store
                .complete_operation(done.key.clone(), decision.clone(), &owner("a"))
                .unwrap();
            let (result_mine, mut result_theirs) = (result("caller"), result("caller"));
            result_theirs.key.tool_call_id = "call-theirs".to_owned();
            store.claim_processed_result(result_mine.clone(), &lease("a")).unwrap();
            store
                .claim_processed_result(result_theirs.clone(), &lease("b"))
                .unwrap();

            store
                .release_receipts(
                    &owner("a"),
                    &[mine.key.clone(), theirs.key.clone(), done.key.clone()],
                    &[result_mine.key.clone(), result_theirs.key.clone()],
                )
                .expect("the release lands");
            let rows = |sql: &str| -> Vec<(String, String, Option<String>)> {
                let connection = store.lock();
                let mut statement = connection.prepare(sql).expect("the query prepares");
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                    .expect("the rows read")
                    .collect::<Result<_, _>>()
                    .expect("every row reads")
            };
            let row =
                |id: &str, status: &str, holder: &str| (id.to_owned(), status.to_owned(), Some(holder.to_owned()));
            assert_eq!(
                rows("SELECT operation_id, status, lease_owner FROM operations ORDER BY operation_id"),
                [
                    row("op-done", "complete", "a"),
                    row("op-mine", "pending", "a"),
                    row("op-theirs", "pending", "b")
                ],
                "a release deletes nothing"
            );
            assert_eq!(
                rows("SELECT tool_call_id, status, lease_owner FROM processed_results ORDER BY tool_call_id"),
                [row("call-1", "pending", "a"), row("call-theirs", "pending", "b")]
            );
            let input: String = store
                .lock()
                .query_row(
                    "SELECT input FROM operations WHERE operation_id = 'op-mine'",
                    [],
                    |row| row.get(0),
                )
                .expect("the released input reads");
            assert_eq!(
                input,
                r#"{"version":1,"binding":"caller","semantic":{"offer_id":"0123456789abcdef"},"context":{"hook":"pre"}}"#,
                "a released receipt keeps the input it recorded"
            );

            assert_eq!(
                store.claim_operation(mine, &lease("c")).unwrap(),
                OperationClaim::Recovered,
                "released work is recovered at once"
            );
            assert_eq!(
                store.claim_processed_result(result_mine, &lease("c")).unwrap(),
                ProcessedResultClaim::Recovered
            );
            assert_eq!(
                store.claim_operation(theirs, &lease("c")).unwrap(),
                OperationClaim::Held,
                "another owner's lease is untouched"
            );
            assert_eq!(
                store.claim_processed_result(result_theirs, &lease("c")).unwrap(),
                ProcessedResultClaim::Held
            );
            assert_eq!(
                store.claim_operation(done, &lease("c")).unwrap(),
                OperationClaim::Complete { decision }
            );
        }
    }

    /// A pending receipt written before receipts carried leases has no time left, so the next
    /// claim recovers it.
    #[test]
    fn a_pending_receipt_without_a_lease_is_recovered() {
        for (store, _dir) in stores() {
            store
                .lock()
                .execute(
                    "INSERT INTO operations (organization_id, caller_id, session_id, operation_id, root, input, status)
                     VALUES ('org', 'caller', 'session', 'op-1', 'root', ?1, 'pending')",
                    [r#"{"version":1,"binding":"caller","semantic":{"offer_id":"0123456789abcdef"}}"#],
                )
                .expect("the fixture row inserts");
            execute(
                &store,
                "INSERT INTO processed_results (organization_id, caller_id, session_id, tool_call_id, root, status)
                 VALUES ('org', 'caller', 'session', 'call-1', 'root', 'pending')",
            );
            assert_eq!(
                store
                    .claim_operation(operation(caller_bound("caller")), &lease("a"))
                    .unwrap(),
                OperationClaim::Recovered
            );
            assert_eq!(
                store.claim_processed_result(result("caller"), &lease("a")).unwrap(),
                ProcessedResultClaim::Recovered
            );
            assert_eq!(lease_of(&store, "operations").0.as_deref(), Some("a"));
            assert_eq!(lease_of(&store, "processed_results").0.as_deref(), Some("a"));
        }
    }

    #[test]
    fn a_completed_result_records_when_it_completed() {
        for (store, _dir) in stores() {
            let request = result("caller");
            store.claim_processed_result(request.clone(), &lease("a")).unwrap();
            let completed_at = |store: &LogStore| -> Option<i64> {
                store
                    .lock()
                    .query_row("SELECT completed_at FROM processed_results", [], |row| row.get(0))
                    .expect("the completion time reads")
            };
            assert_eq!(completed_at(&store), None);
            store
                .complete_processed_result(
                    request.key,
                    "approved".to_owned(),
                    serde_json::json!({"decision": "allow"}),
                    &owner("a"),
                )
                .unwrap();
            assert!(completed_at(&store).is_some());
        }
    }

    #[test]
    fn a_corrupt_operation_row_is_a_storage_failure() {
        let request = || operation(session_bound("caller"));
        let decision = serde_json::json!({"decision": "allow_call"});
        for corruption in [
            "UPDATE operations SET status='bogus'",
            "UPDATE operations SET input='not json'",
            "UPDATE operations SET status='complete', decision='not json'",
        ] {
            for (store, _dir) in stores() {
                store
                    .claim_operation(request(), &lease("a"))
                    .expect("the operation claims");
                execute(&store, corruption);
                assert!(
                    matches!(
                        store.claim_operation(request(), &lease("a")),
                        Err(ReceiptError::Storage(_))
                    ),
                    "{corruption}"
                );
                assert!(
                    matches!(
                        store.complete_operation(request().key, decision.clone(), &owner("a")),
                        Err(ReceiptError::Storage(_))
                    ),
                    "{corruption}"
                );
            }
        }
        for (store, _dir) in stores() {
            store
                .claim_operation(request(), &lease("a"))
                .expect("the operation claims");
            execute(&store, "UPDATE operations SET status='complete'");
            assert!(matches!(
                store.claim_operation(request(), &lease("a")),
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
                store
                    .claim_processed_result(request(), &lease("a"))
                    .expect("the result claims");
                execute(&store, corruption);
                assert!(
                    matches!(
                        store.claim_processed_result(request(), &lease("a")),
                        Err(ReceiptError::Storage(_))
                    ),
                    "{corruption}"
                );
                assert!(
                    matches!(
                        store.complete_processed_result(
                            request().key,
                            "approved".to_owned(),
                            decision.clone(),
                            &owner("a")
                        ),
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
                store
                    .claim_processed_result(request(), &lease("a"))
                    .expect("the result claims");
                execute(&store, corruption);
                assert!(
                    matches!(
                        store.claim_processed_result(request(), &lease("a")),
                        Err(ReceiptError::Storage(_))
                    ),
                    "{corruption}"
                );
            }
        }
    }
}
