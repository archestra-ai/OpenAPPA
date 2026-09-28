//! The stored batch encoding, and the read that rebuilds a [`Log`] from its batches. Both
//! backends store and read the same bytes.

use appa_engine::fact::{Fact, TrajectoryOpening};
use appa_engine::profile::PolicyFileKey;
use appa_engine::value::TrajectoryId;

use crate::{HostObservation, HostRecord, Log, ReadError};

/// One stored batch. Both streams share a position, so an engine decision and the host
/// observation it belongs with are durable together or not at all.
///
/// The encoding is the shape: a batch carrying no host observation is the bare JSON array of
/// its facts, and one carrying an observation is an object with both fields. Nothing sniffs
/// between unrelated payloads — the first token settles which of the two a stored row is.
struct Record {
    facts: Vec<Fact>,
    host: Option<HostObservation>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostRow {
    #[serde(default)]
    facts: Vec<Fact>,
    host: HostObservation,
}

#[derive(serde::Serialize)]
struct HostRowRef<'a> {
    facts: &'a [Fact],
    host: &'a HostObservation,
}

pub(crate) fn encode(facts: &[Fact], host: Option<&HostObservation>) -> Vec<u8> {
    let expectation = "records serialize: every field is a serde type with no float or map key";
    match host {
        None => serde_json::to_vec(facts).expect(expectation),
        Some(host) => serde_json::to_vec(&HostRowRef { facts, host }).expect(expectation),
    }
}

/// Which of the two shapes a stored row is, from its first token. A row that is neither —
/// an older encoding, or bytes this build cannot read — refuses the whole log.
fn decode(bytes: &[u8]) -> Result<Record, ReadError> {
    let undecodable = |error: serde_json::Error| ReadError::Undecodable(error.to_string());
    match bytes.iter().find(|byte| !byte.is_ascii_whitespace()) {
        Some(b'[') => serde_json::from_slice(bytes)
            .map(|facts| Record { facts, host: None })
            .map_err(undecodable),
        Some(b'{') => serde_json::from_slice::<HostRow>(bytes)
            .map(|row| Record {
                facts: row.facts,
                host: Some(row.host),
            })
            .map_err(undecodable),
        _ => Err(ReadError::Undecodable(
            "a stored batch is neither an engine batch nor a host record".to_string(),
        )),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("event sequence contains a gap: expected position {expected}, found {found}")]
pub(crate) struct SequenceGap {
    expected: u64,
    found: i64,
}

/// The batches of rows read in `seq` order, refused unless their positions run 0, 1, 2, …
pub(crate) fn contiguous(rows: Vec<(i64, Vec<u8>)>) -> Result<Vec<Vec<u8>>, SequenceGap> {
    rows.into_iter()
        .enumerate()
        .map(|(expected, (found, batch))| match found == expected as i64 {
            true => Ok(batch),
            false => Err(SequenceGap {
                expected: expected as u64,
                found,
            }),
        })
        .collect()
}

/// The policy file key a log's opening names. A log with no batch is an unknown root.
pub(crate) fn opening_key(root: &TrajectoryId, batches: &[Vec<u8>]) -> Result<PolicyFileKey, ReadError> {
    let Some(first) = batches.first() else {
        return Err(ReadError::UnknownRoot {
            root: root.as_str().to_string(),
        });
    };
    match decode(first)?.facts.into_iter().next() {
        Some(Fact::TrajectoryOpened(TrajectoryOpening { policy_file_key, .. })) => Ok(policy_file_key),
        _ => Err(ReadError::Undecodable(
            "the log does not open with a TrajectoryOpened record".to_string(),
        )),
    }
}

pub(crate) fn decoded(root: &TrajectoryId, batches: Vec<Vec<u8>>, policy_file: Vec<u8>) -> Result<Log, ReadError> {
    let basis = batches.len() as u64;
    let mut facts = Vec::new();
    let mut host = Vec::new();
    for (seq, batch) in batches.iter().enumerate() {
        let record = decode(batch)?;
        facts.extend(record.facts);
        if let Some(observation) = record.host {
            host.push(HostRecord {
                seq: seq as u64,
                observation,
            });
        }
    }
    Ok(Log {
        root: root.clone(),
        facts,
        basis,
        policy_file,
        host,
    })
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use appa_engine::value::DispatchId;
    use appa_runtime_api::Ruling;

    use super::*;
    use crate::HostActor;
    use crate::tests::{observed, punctuation, root};

    /// One observation of every kind, in declaration order, so a golden covers each wire tag.
    fn golden_observations() -> Vec<HostObservation> {
        let actor = HostActor {
            root: root(),
            child: Some(TrajectoryId::new("cc:child")),
        };
        vec![
            observed(root().as_str(), "demo"),
            HostObservation::CallBound {
                trajectory: root(),
                call_id: "toolu_1".to_string(),
                dispatch: DispatchId::new(
                    root(),
                    serde_json::from_value(serde_json::json!("ab".repeat(32))).expect("a digest decodes"),
                    7,
                ),
            },
            HostObservation::Vouched {
                actor: actor.clone(),
                key: "offer:one".to_string(),
                ruling: Some(Ruling::Approve),
            },
            HostObservation::Claimed {
                actor: actor.clone(),
                key: "offer:one".to_string(),
                until: SystemTime::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 5),
            },
            HostObservation::Released {
                actor: actor.clone(),
                key: "offer:one".to_string(),
            },
            HostObservation::PromptSeen { actor: actor.clone() },
            HostObservation::PromptSettled {
                actor: HostActor {
                    root: root(),
                    child: None,
                },
            },
            HostObservation::TurnEnded { actor },
        ]
    }

    /// The stored batch bytes are a persisted format: every host observation kind, the bare
    /// fact array, and an observation with no facts, byte for byte.
    #[test]
    fn stored_batch_bytes_are_frozen() {
        let facts = r#"[{"Boundary":{"trajectory":"cc:root","kind":"VoidReturn"}}]"#;
        let hosts = [
            r#"{"kind":"inventory","actor":"cc:root","adapter":"kagent","inventory":{"tools":[{"name":"read","tool":"mcp:demo/read"}],"sources":[]}}"#,
            r#"{"kind":"call_bound","trajectory":"cc:root","call_id":"toolu_1","dispatch":{"trajectory":"cc:root","digest":"abababababababababababababababababababababababababababababababab","occurrence":7}}"#,
            r#"{"kind":"vouched","actor":{"root":"cc:root","child":"cc:child"},"key":"offer:one","ruling":"approve"}"#,
            r#"{"kind":"claimed","actor":{"root":"cc:root","child":"cc:child"},"key":"offer:one","until":{"secs_since_epoch":1700000000,"nanos_since_epoch":5}}"#,
            r#"{"kind":"released","actor":{"root":"cc:root","child":"cc:child"},"key":"offer:one"}"#,
            r#"{"kind":"prompt_seen","actor":{"root":"cc:root","child":"cc:child"}}"#,
            r#"{"kind":"prompt_settled","actor":{"root":"cc:root","child":null}}"#,
            r#"{"kind":"turn_ended","actor":{"root":"cc:root","child":"cc:child"}}"#,
        ];
        let observations = golden_observations();
        assert_eq!(observations.len(), hosts.len());

        let bare = encode(&punctuation(), None);
        assert_eq!(String::from_utf8(bare.clone()).unwrap(), facts);
        assert!(matches!(decode(&bare), Ok(Record { facts, host: None }) if facts == punctuation()));

        for (observation, host) in observations.iter().zip(hosts) {
            let with_facts = encode(&punctuation(), Some(observation));
            assert_eq!(
                String::from_utf8(with_facts.clone()).unwrap(),
                format!(r#"{{"facts":{facts},"host":{host}}}"#)
            );
            assert!(matches!(
                decode(&with_facts),
                Ok(Record { facts, host: Some(decoded) }) if facts == punctuation() && &decoded == observation
            ));
            let alone = encode(&[], Some(observation));
            assert_eq!(
                String::from_utf8(alone.clone()).unwrap(),
                format!(r#"{{"facts":[],"host":{host}}}"#)
            );
            assert!(matches!(
                decode(&alone),
                Ok(Record { facts, host: Some(decoded) }) if facts.is_empty() && &decoded == observation
            ));
        }
    }

    /// Decode settles the shape on the first non-whitespace byte, and an object may omit
    /// its facts.
    #[test]
    fn decode_dispatches_on_the_first_non_whitespace_byte() {
        let facts = r#"[{"Boundary":{"trajectory":"cc:root","kind":"VoidReturn"}}]"#;
        let host = r#"{"kind":"prompt_seen","actor":{"root":"cc:root","child":null}}"#;
        let seen = HostObservation::PromptSeen {
            actor: HostActor {
                root: root(),
                child: None,
            },
        };
        assert!(matches!(
            decode(format!(" \n\t{facts}").as_bytes()),
            Ok(Record { facts, host: None }) if facts == punctuation()
        ));
        assert!(matches!(
            decode(format!("\r\n {{\"host\":{host}}}").as_bytes()),
            Ok(Record { facts, host: Some(decoded) }) if facts.is_empty() && decoded == seen
        ));
        for row in [b"".as_slice(), b"   ", br#""text""#, b"42", b"null"] {
            assert!(
                matches!(decode(row), Err(ReadError::Undecodable(_))),
                "{}",
                String::from_utf8_lossy(row)
            );
        }
    }
}
