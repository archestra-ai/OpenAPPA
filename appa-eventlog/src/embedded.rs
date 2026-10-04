//! Embedded peer messages for one family. An unread body lives here until admission.
//! The runtime supplies the sender's captured label. Embedding hosts use the Runtime
//! API, rather than these storage methods, to authorize transfers and reads.
//!
//! The 24-hour TTL bounds the unread inbox. Expire nulls the body of a `held` or `direct`
//! row that can no longer be returned, and leaves the proof row. A completed `read`
//! decision is a durable processed-result receipt for exact-call replay. It is not unread
//! inbox data, and the TTL does not erase it or anything in the trajectory log.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use appa_engine::label::Label;
use appa_runtime_api::PeerDigest;
use serde_json::Value;

pub(crate) const BODY_LIMIT: usize = 64 * 1024;
pub(crate) const UNREAD_QUOTA: usize = 32;
pub(crate) const TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedRow {
    pub id: String,
    pub sender: String,
    pub recipient: String,
    pub pending_spawn: Option<String>,
    pub dispatch: String,
    pub digest: String,
    pub label: Label,
    pub body: Option<String>,
    pub status: EmbeddedStatus,
    pub read_call_id: Option<String>,
    pub read_arguments: Option<String>,
    pub decision: Option<String>,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedStatus {
    Held,
    Direct,
    Read,
}

impl EmbeddedStatus {
    fn parse(text: &str) -> Result<Self, EmbeddedError> {
        match text {
            "held" => Ok(Self::Held),
            "direct" => Ok(Self::Direct),
            "read" => Ok(Self::Read),
            other => Err(EmbeddedError::Storage(format!("unknown embedded peer status {other}"))),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NewEmbedded {
    pub id: String,
    pub root: String,
    pub sender: String,
    pub recipient: String,
    pub pending_spawn: Option<String>,
    pub dispatch: String,
    pub digest: String,
    pub label: serde_json::Value,
    pub body: String,
    pub expires_at: i64,
    pub created_at: i64,
}

impl NewEmbedded {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fresh(
        root: &str,
        sender: &str,
        recipient: &str,
        pending_spawn: Option<&str>,
        dispatch: &str,
        digest: &PeerDigest,
        label: &Label,
        body: &str,
        now: SystemTime,
    ) -> Result<Self, EmbeddedError> {
        if body.is_empty() || body.len() > BODY_LIMIT {
            return Err(EmbeddedError::TooLarge);
        }
        let created_at = millis(now);
        Ok(Self {
            id: uuid::Uuid::new_v4().hyphenated().to_string(),
            root: root.to_owned(),
            sender: sender.to_owned(),
            recipient: recipient.to_owned(),
            pending_spawn: pending_spawn.map(str::to_owned),
            dispatch: dispatch.to_owned(),
            digest: digest.to_string(),
            label: serde_json::to_value(label).map_err(|error| EmbeddedError::Storage(error.to_string()))?,
            body: body.to_owned(),
            expires_at: created_at.saturating_add(i64::try_from(TTL.as_millis()).unwrap_or(i64::MAX)),
            created_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum EmbeddedClaim {
    Stored(EmbeddedRow),
    Conflict,
    Quota,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum DirectTake {
    Taken(EmbeddedRow),
    Already(EmbeddedRow),
    Busy,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadClaim {
    pub row: EmbeddedRow,
    pub generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum ReadTake {
    Ready(ReadClaim),
    Done(EmbeddedRow),
    /// This call id is already claimed, or it names different arguments.
    Busy,
    Missing,
}

#[derive(Debug, thiserror::Error)]
pub enum EmbeddedError {
    #[error("embedded peer storage failed: {0}")]
    Storage(String),
    #[error("an embedded peer message exceeds the byte limit")]
    TooLarge,
}

pub(crate) fn millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The claim an embedded-read append must still own. `bind` moves an opening ticket to bound
/// in the same transaction as a dispatch open or an admission.
pub struct EmbeddedAppendHold<'a> {
    pub id: &'a str,
    pub call_id: &'a str,
    pub generation: i64,
    pub bind: bool,
}

pub(crate) const TICKET_OPENING: &str = "opening";
pub(crate) const TICKET_BOUND: &str = "bound";

/// Canonical ticket text. SQLite compares this exact string. PostgreSQL compares the same
/// object as jsonb, so key order there does not matter.
pub(crate) fn read_ticket(kind: &str, generation: i64) -> String {
    serde_json::json!({"generation": generation, "kind": kind}).to_string()
}

pub(crate) fn is_receipt(decision: Option<&str>) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(decision.unwrap_or("")) else {
        return false;
    };
    matches!(
        value.get("kind").and_then(|kind| kind.as_str()),
        Some("deliver" | "replace")
    )
}

pub(crate) fn ticket_kind(decision: Option<&str>) -> Option<(String, i64)> {
    let value: Value = serde_json::from_str(decision?).ok()?;
    let kind = value.get("kind")?.as_str()?;
    if kind != TICKET_OPENING && kind != TICKET_BOUND {
        return None;
    }
    Some((kind.to_string(), value.get("generation")?.as_i64()?))
}

pub(crate) fn next_read_ticket(decision: Option<&str>) -> Result<(String, i64), EmbeddedError> {
    let Some(decision) = decision else {
        return Ok((TICKET_OPENING.to_string(), 1));
    };
    let (kind, generation) = ticket_kind(Some(decision))
        .filter(|(_, generation)| *generation > 0)
        .ok_or_else(|| EmbeddedError::Storage("invalid embedded read ticket".to_string()))?;
    let next = generation
        .checked_add(1)
        .ok_or_else(|| EmbeddedError::Storage("embedded read generation exhausted".to_string()))?;
    Ok((kind, next))
}

pub(crate) fn classify_read(row: &EmbeddedRow, call_id: &str, arguments: &str) -> Option<ReadTake> {
    match row.status {
        EmbeddedStatus::Direct => Some(ReadTake::Busy),
        EmbeddedStatus::Read if row.read_call_id.as_deref() == Some(call_id) => {
            if row.read_arguments.as_deref() != Some(arguments) {
                Some(ReadTake::Busy)
            } else if is_receipt(row.decision.as_deref()) {
                Some(ReadTake::Done(row.clone()))
            } else {
                None
            }
        }
        EmbeddedStatus::Read => Some(ReadTake::Busy),
        EmbeddedStatus::Held => None,
    }
}

pub(crate) fn same_payload(row: &EmbeddedRow, fresh: &NewEmbedded) -> bool {
    row.sender == fresh.sender
        && row.recipient == fresh.recipient
        && row.pending_spawn == fresh.pending_spawn
        && row.digest == fresh.digest
}

pub(crate) struct StoredEmbedded {
    pub id: String,
    pub sender: String,
    pub recipient: String,
    pub pending_spawn: Option<String>,
    pub dispatch: String,
    pub digest: String,
    pub label: serde_json::Value,
    pub body: Option<String>,
    pub status: String,
    pub read_call_id: Option<String>,
    pub read_arguments: Option<String>,
    pub decision: Option<String>,
    pub expires_at: i64,
}

impl StoredEmbedded {
    pub(crate) fn decode(self) -> Result<EmbeddedRow, EmbeddedError> {
        Ok(EmbeddedRow {
            id: self.id,
            sender: self.sender,
            recipient: self.recipient,
            pending_spawn: self.pending_spawn,
            dispatch: self.dispatch,
            digest: self.digest,
            label: serde_json::from_value(self.label).map_err(|error| EmbeddedError::Storage(error.to_string()))?,
            body: self.body,
            status: EmbeddedStatus::parse(&self.status)?,
            read_call_id: self.read_call_id,
            read_arguments: self.read_arguments,
            decision: self.decision,
            expires_at: self.expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use appa_engine::label::Label;
    use appa_runtime_api::{PeerDigest, TrajectoryId};

    use crate::{Backend, LogStore};

    use super::*;

    #[test]
    fn resuming_a_read_refuses_corrupt_or_exhausted_tickets() {
        assert_eq!(next_read_ticket(None).unwrap(), (TICKET_OPENING.to_string(), 1));
        assert_eq!(
            next_read_ticket(Some(&read_ticket(TICKET_BOUND, 3))).unwrap(),
            (TICKET_BOUND.to_string(), 4)
        );
        for ticket in [
            "not json".to_string(),
            r#"{"kind":"unknown","generation":1}"#.to_string(),
            r#"{"kind":"opening"}"#.to_string(),
            read_ticket(TICKET_OPENING, 0),
            read_ticket(TICKET_BOUND, -1),
            read_ticket(TICKET_BOUND, i64::MAX),
        ] {
            assert!(next_read_ticket(Some(&ticket)).is_err(), "accepted {ticket}");
        }
    }

    fn claim(store: &LogStore, dispatch: &str, body: &str) -> Result<EmbeddedClaim, EmbeddedError> {
        store.claim_embedded_peer(
            &TrajectoryId("root".to_string()),
            &TrajectoryId("sender".to_string()),
            &TrajectoryId("recipient".to_string()),
            None,
            dispatch,
            &PeerDigest::of_body(body),
            &Label::top(),
            body,
            SystemTime::now(),
        )
    }

    #[test]
    fn a_dispatch_retries_at_quota_and_a_changed_body_conflicts() {
        let store = LogStore::open(Backend::Memory).expect("memory opens");
        let first = claim(&store, "dispatch-1", "one").expect("the first send stores");
        let EmbeddedClaim::Stored(row) = &first else {
            panic!("the first send stores, got {first:?}");
        };
        let id = row.id.clone();
        for index in 2..=UNREAD_QUOTA {
            let claimed = claim(&store, &format!("dispatch-{index}"), "body").expect("a send stores");
            assert!(matches!(claimed, EmbeddedClaim::Stored(_)));
        }
        assert!(matches!(
            claim(&store, "dispatch-extra", "overflow"),
            Ok(EmbeddedClaim::Quota)
        ));
        let retried = claim(&store, "dispatch-1", "one").expect("the original dispatch retries");
        let EmbeddedClaim::Stored(again) = retried else {
            panic!("the retry is the same record");
        };
        assert_eq!(again.id, id);
        assert!(matches!(
            claim(&store, "dispatch-1", "changed"),
            Ok(EmbeddedClaim::Conflict)
        ));
        let listed = store
            .list_embedded_peer(
                &TrajectoryId("root".to_string()),
                &TrajectoryId("recipient".to_string()),
                SystemTime::now(),
            )
            .expect("the inbox lists");
        assert_eq!(listed.len(), UNREAD_QUOTA);
        assert!(listed.iter().all(|row| row.body.is_some()));
    }
}
