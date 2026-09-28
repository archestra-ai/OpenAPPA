//! Idempotent operation and processed-result receipts.
//!
//! These records are not engine facts. They are host integration state: whether an operation
//! or processed result is claimed, pending, or complete. SQLite and Memory keep the rows in the
//! same database as the log; PostgreSQL hosts install the equivalent `openappa_*` tables
//! through their own migrations.

use appa_engine::value::TrajectoryId;
use serde_json::Value;

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
    Claimed,
    Complete { decision: Value },
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

#[derive(Debug, Clone, PartialEq)]
pub enum ProcessedResultClaim {
    Claimed,
    Complete { approved_output: String, decision: Value },
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

/// One operation receipt row, its input envelope decoded.
pub(crate) struct StoredOperation {
    pub(crate) session: SessionScope,
    pub(crate) binding: ReceiptBinding,
    pub(crate) root: String,
    pub(crate) semantic: Value,
    pub(crate) status: String,
    pub(crate) decision: Option<StoredJson>,
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
}

/// What a completion does to the row it found.
pub(crate) enum Completion {
    /// The receipt was pending: record the completion.
    Write,
    /// The receipt already holds this very completion.
    Unchanged,
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
    if !stored.owned_by(key) {
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
    if stored.session != key.session {
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
        Self::Storage(detail.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Backend, LogStore};

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
            let mut request = operation(caller_bound("caller"));
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
                .claim_operation(operation(session_bound("caller")))
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
                store.claim_operation(request.clone()),
                Err(ReceiptError::Storage(_))
            ));
            assert!(matches!(
                store.complete_operation(request.key, serde_json::json!({"decision": "allow_call"})),
                Err(ReceiptError::Storage(_))
            ));
        }
    }

    #[test]
    fn operation_receipts_refuse_another_scope_without_writing() {
        for (store, _dir) in stores() {
            let owned = operation(caller_bound("caller"));
            store.claim_operation(owned.clone()).expect("the operation claims");

            let other_caller = operation(caller_bound("other"));
            let session_bound = operation(session_bound("caller"));
            let mut other_root = owned.clone();
            other_root.root = TrajectoryId::new("other-root");
            for request in [&other_caller, &session_bound, &other_root] {
                assert!(matches!(
                    store.claim_operation(request.clone()),
                    Err(ReceiptError::ScopeMismatch)
                ));
            }
            for key in [&other_caller.key, &session_bound.key] {
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
                .claim_operation(operation(session_bound("caller")))
                .expect("the operation claims");
            assert!(matches!(
                store.claim_operation(operation(session_bound("other"))),
                Err(ReceiptError::Pending)
            ));
            store
                .complete_operation(
                    operation(session_bound("other")).key,
                    serde_json::json!({"decision": "allow_call"}),
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
                store.claim_operation(request).expect("the operation claims");
            }
            let mut completed_elsewhere = operation(session_bound("other"));
            completed_elsewhere.key.operation_id = "op-session".to_owned();
            store
                .complete_operation(completed_elsewhere.key, serde_json::json!({"decision": "allow_call"}))
                .expect("another caller in the session completes");
            assert_eq!(
                caller_of(&store, "operations"),
                [Some("caller".to_owned()), Some("session-caller".to_owned()), None]
            );

            let mut callerless_result = result("caller");
            callerless_result.key.caller_id = None;
            callerless_result.key.tool_call_id = "call-callerless".to_owned();
            for request in [result("caller"), callerless_result] {
                store.claim_processed_result(request).expect("the result claims");
            }
            store
                .complete_processed_result(
                    result("other").key,
                    "approved".to_owned(),
                    serde_json::json!({"decision": "allow"}),
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
            store.claim_operation(request.clone()).expect("the operation claims");
            assert!(pending("root"), "a pending operation");
            assert!(!pending("other-root"));
            store
                .complete_operation(request.key, serde_json::json!({"decision": "allow_call"}))
                .expect("the operation completes");
            assert!(!pending("root"));

            let request = result("caller");
            store
                .claim_processed_result(request.clone())
                .expect("the result claims");
            assert!(pending("root"), "a pending processed result");
            assert!(!pending("other-root"));
            store
                .complete_processed_result(
                    request.key,
                    "approved".to_owned(),
                    serde_json::json!({"decision": "allow"}),
                )
                .expect("the result completes");
            assert!(!pending("root"));
        }
    }

    #[test]
    fn processed_results_refuse_another_scope_without_writing() {
        for (store, _dir) in stores() {
            let owned = result("caller");
            store.claim_processed_result(owned.clone()).expect("the result claims");

            let mut other_root = owned.clone();
            other_root.root = TrajectoryId::new("other-root");
            assert!(matches!(
                store.claim_processed_result(other_root),
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
    fn a_claimed_receipt_refuses_a_second_claim_and_other_input() {
        for (store, _dir) in stores() {
            let request = operation(caller_bound("caller"));
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
                store.complete_operation(operation(session_bound("caller")).key, serde_json::json!({})),
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

            let request = operation(caller_bound("caller"));
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
        let request = || operation(session_bound("caller"));
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
}
