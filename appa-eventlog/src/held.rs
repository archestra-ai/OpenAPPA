//! Peer messages held for a receiving root until it takes them.
//!
//! A held message is host integration state, not an engine fact: the body lives only here,
//! never in the log. Rows are scoped by the receiving root, so one root never sees or takes
//! another's message. Every row carries the label its sender attached and an expiry; an
//! expired row reads as absent.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use appa_engine::label::Label;
use appa_runtime_api::PeerDigest;

/// The store's identity for one held message: a UUID v4 in its hyphenated lowercase form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HeldPeerId(String);

impl HeldPeerId {
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::new_v4().hyphenated().to_string())
    }

    pub fn parse(text: &str) -> Result<Self, HeldError> {
        uuid::Uuid::try_parse(text)
            .map(|id| Self(id.hyphenated().to_string()))
            .map_err(|_| HeldError::MalformedId(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HeldPeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for HeldPeerId {
    type Error = HeldError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<HeldPeerId> for String {
    fn from(id: HeldPeerId) -> Self {
        id.0
    }
}

/// What a receiver learns of a held message before it takes the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldNotice {
    pub id: HeldPeerId,
    pub digest: PeerDigest,
    pub label: Label,
    pub expires: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldPeerMessage {
    pub notice: HeldNotice,
    pub body: String,
}

#[derive(Debug, thiserror::Error)]
pub enum HeldError {
    #[error("held peer message storage failed: {0}")]
    Storage(String),
    #[error("a held peer message row does not decode: {0}")]
    Corrupt(String),
    #[error("{0:?} is not a held peer message id")]
    MalformedId(String),
}

/// One held row as both backends read it, before its columns are checked.
pub(crate) struct StoredNotice {
    pub(crate) id: String,
    pub(crate) digest: String,
    pub(crate) label: serde_json::Value,
    pub(crate) expires_at: i64,
}

impl StoredNotice {
    pub(crate) fn decode(self) -> Result<HeldNotice, HeldError> {
        let corrupt = |detail: String| HeldError::Corrupt(detail);
        Ok(HeldNotice {
            id: HeldPeerId::parse(&self.id).map_err(|error| corrupt(error.to_string()))?,
            digest: PeerDigest::parse(&self.digest).map_err(|error| corrupt(error.to_string()))?,
            label: serde_json::from_value(self.label).map_err(|error| corrupt(error.to_string()))?,
            expires: UNIX_EPOCH
                + Duration::from_millis(
                    u64::try_from(self.expires_at).map_err(|_| corrupt(format!("expiry {}", self.expires_at)))?,
                ),
        })
    }
}

/// A new row's columns: its id, its label as JSON, and its expiry in unix milliseconds.
pub(crate) struct NewHeld {
    pub(crate) id: HeldPeerId,
    pub(crate) label: serde_json::Value,
    pub(crate) expires_at: i64,
}

impl NewHeld {
    pub(crate) fn new(label: &Label, now: SystemTime, ttl: Duration) -> Result<Self, HeldError> {
        Ok(Self {
            id: HeldPeerId::fresh(),
            label: serde_json::to_value(label).map_err(|error| HeldError::Storage(error.to_string()))?,
            expires_at: millis(now).saturating_add(i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX)),
        })
    }

    pub(crate) fn notice(self, digest: PeerDigest, label: &Label) -> HeldNotice {
        HeldNotice {
            id: self.id,
            digest,
            label: label.clone(),
            expires: UNIX_EPOCH + Duration::from_millis(self.expires_at as u64),
        }
    }
}

/// Unix milliseconds, clamped to the column's range.
pub(crate) fn millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub(crate) fn quota_limit(quota: usize) -> i64 {
    i64::try_from(quota).unwrap_or(i64::MAX)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{Backend, LogStore, TrajectoryId};
    use appa_engine::label::{Audience, Trust};

    const TTL: Duration = Duration::from_secs(60);
    const QUOTA: usize = 8;

    fn stores() -> Vec<(LogStore, Option<tempfile::TempDir>)> {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let file = LogStore::open(Backend::Sqlite {
            path: dir.path().join("appa.db"),
        })
        .expect("a fresh file store opens");
        let memory = LogStore::open(Backend::Memory).expect("an in-memory store opens");
        vec![(file, Some(dir)), (memory, None)]
    }

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds)
    }

    fn label(rank: u8) -> Label {
        Label::new(Trust::new(rank), Audience::public())
    }

    fn hold(store: &LogStore, receiver: &TrajectoryId, body: &str, now: SystemTime) -> HeldNotice {
        store
            .hold_peer_message(receiver, PeerDigest::of_body(body), &label(1), body, now, TTL, QUOTA)
            .expect("the message is held")
    }

    /// Hold, notify once, peek, then take the body exactly once. `suffix` keeps the receivers
    /// of one run apart from any other run on a shared database.
    pub(crate) fn a_held_message_is_notified_once_and_taken_once(store: &LogStore, suffix: &str) {
        let receiver = TrajectoryId::new(format!("held-receiver:{suffix}"));
        let first = store
            .hold_peer_message(
                &receiver,
                PeerDigest::of_body("one"),
                &label(2),
                "one",
                at(0),
                TTL,
                QUOTA,
            )
            .expect("the message is held");
        assert_eq!(first.digest, PeerDigest::of_body("one"));
        assert_eq!(first.label, label(2));
        assert_eq!(first.expires, at(60));
        let second = hold(store, &receiver, "two", at(1));

        assert_eq!(
            store.peer_notices(&receiver, at(2)).expect("the notices read"),
            vec![first.clone(), second.clone()],
            "oldest first"
        );
        assert_eq!(
            store.peer_notices(&receiver, at(2)).expect("the notices read"),
            Vec::new(),
            "a notice is given once"
        );
        let third = hold(store, &receiver, "three", at(3));
        assert_eq!(
            store.peer_notices(&receiver, at(3)).expect("the notices read"),
            vec![third.clone()]
        );

        assert_eq!(
            store
                .peek_peer_message(&receiver, &first.id, at(4))
                .expect("the peek reads"),
            Some(first.clone())
        );
        assert_eq!(
            store
                .take_peer_message(&receiver, &first.id, at(4))
                .expect("the take reads"),
            Some(HeldPeerMessage {
                notice: first.clone(),
                body: "one".to_owned(),
            })
        );
        assert_eq!(
            store
                .take_peer_message(&receiver, &first.id, at(4))
                .expect("the take reads"),
            None
        );
        assert_eq!(
            store
                .peek_peer_message(&receiver, &first.id, at(4))
                .expect("the peek reads"),
            None
        );
        for notice in [second, third] {
            assert!(
                store
                    .take_peer_message(&receiver, &notice.id, at(5))
                    .expect("the take reads")
                    .is_some()
            );
        }
    }

    #[test]
    fn sqlite_a_held_message_is_notified_once_and_taken_once() {
        for (store, _dir) in stores() {
            a_held_message_is_notified_once_and_taken_once(&store, "sqlite");
        }
    }

    #[test]
    fn an_expired_message_is_hidden_and_deleted() {
        for (store, _dir) in stores() {
            let receiver = TrajectoryId::new("receiver");
            let held = hold(&store, &receiver, "late", at(0));
            assert_eq!(
                store.peer_notices(&receiver, at(60)).expect("the notices read"),
                Vec::new()
            );
            assert_eq!(
                store
                    .peek_peer_message(&receiver, &held.id, at(60))
                    .expect("the peek reads"),
                None
            );
            assert_eq!(
                store
                    .take_peer_message(&receiver, &held.id, at(61))
                    .expect("the take reads"),
                None
            );
            assert_eq!(
                store
                    .peek_peer_message(&receiver, &held.id, at(0))
                    .expect("the peek reads"),
                None,
                "the expired take deleted the row"
            );

            let stale = hold(&store, &receiver, "stale", at(100));
            hold(&store, &receiver, "fresh", at(200));
            assert_eq!(
                store
                    .peek_peer_message(&receiver, &stale.id, at(100))
                    .expect("the peek reads"),
                None,
                "a later hold deletes the receiver's expired rows"
            );

            let ended = TrajectoryId::new("ended");
            let unread = hold(&store, &ended, "never read", at(300));
            hold(&store, &receiver, "fresh", at(400));
            assert_eq!(
                store
                    .peek_peer_message(&ended, &unread.id, at(300))
                    .expect("the peek reads"),
                None,
                "a hold for any receiver deletes every receiver's expired rows"
            );
        }
    }

    #[test]
    fn a_hold_beyond_the_quota_drops_the_oldest() {
        for (store, _dir) in stores() {
            let receiver = TrajectoryId::new("receiver");
            let held: Vec<_> = ["a", "b", "c"]
                .iter()
                .map(|body| {
                    store
                        .hold_peer_message(&receiver, PeerDigest::of_body(body), &label(1), body, at(0), TTL, 2)
                        .expect("the message is held")
                })
                .collect();
            assert_eq!(
                store.peer_notices(&receiver, at(1)).expect("the notices read"),
                held[1..].to_vec()
            );
            assert_eq!(
                store
                    .take_peer_message(&receiver, &held[0].id, at(1))
                    .expect("the take reads"),
                None
            );
        }
    }

    #[test]
    fn one_receiver_never_sees_or_takes_anothers_message() {
        for (store, _dir) in stores() {
            let (owner, other) = (TrajectoryId::new("owner"), TrajectoryId::new("other"));
            let held = hold(&store, &owner, "private", at(0));
            for body in ["x", "y"] {
                store
                    .hold_peer_message(&other, PeerDigest::of_body(body), &label(1), body, at(0), TTL, 1)
                    .expect("the other receiver's message is held");
            }

            assert_eq!(
                store.peer_notices(&other, at(1)).expect("the notices read").len(),
                1,
                "a quota counts only its own receiver's rows"
            );
            assert_eq!(
                store
                    .peek_peer_message(&other, &held.id, at(1))
                    .expect("the peek reads"),
                None
            );
            assert_eq!(
                store
                    .take_peer_message(&other, &held.id, at(1))
                    .expect("the take reads"),
                None
            );
            assert_eq!(
                store.peer_notices(&owner, at(1)).expect("the notices read"),
                vec![held.clone()]
            );
            assert_eq!(
                store
                    .take_peer_message(&owner, &held.id, at(1))
                    .expect("the take reads")
                    .map(|message| message.body),
                Some("private".to_owned())
            );
        }
    }

    #[test]
    fn a_corrupt_held_row_is_refused() {
        for corruption in [
            "UPDATE held_peer_messages SET label = 'not json'",
            "UPDATE held_peer_messages SET digest = 'ABC'",
            "UPDATE held_peer_messages SET id = 'not a uuid'",
        ] {
            for (store, _dir) in stores() {
                let receiver = TrajectoryId::new("receiver");
                hold(&store, &receiver, "body", at(0));
                store.lock().execute(corruption, []).expect("the corruption lands");
                assert!(
                    matches!(store.peer_notices(&receiver, at(1)), Err(HeldError::Corrupt(_))),
                    "{corruption}"
                );
            }
        }
    }

    #[test]
    fn a_held_id_is_a_canonical_uuid() {
        let fresh = HeldPeerId::fresh();
        assert_eq!(HeldPeerId::parse(fresh.as_str()).expect("a fresh id parses"), fresh);
        assert_eq!(
            HeldPeerId::parse("6F9619FF-8B86-4D11-B42D-00C04FC964FF")
                .expect("an uppercase id parses")
                .as_str(),
            "6f9619ff-8b86-4d11-b42d-00c04fc964ff"
        );
        assert!(matches!(HeldPeerId::parse("held-1"), Err(HeldError::MalformedId(_))));
        let json = serde_json::to_value(&fresh).expect("an id serializes");
        assert_eq!(
            serde_json::from_value::<HeldPeerId>(json).expect("an id deserializes"),
            fresh
        );
        assert!(serde_json::from_value::<HeldPeerId>(serde_json::json!("held-1")).is_err());
    }
}
