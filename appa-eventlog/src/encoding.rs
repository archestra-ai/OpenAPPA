//! The stored batch encoding, and the read that rebuilds a [`Log`] from its batches. Both
//! backends store and read the same bytes.

use appa_engine::fact::{Fact, TrajectoryOpening};
use appa_engine::profile::{PolicyFileKey, PolicyRevision};
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

/// Which of the two shapes the stored row at `seq` is, from its first token. A row that is
/// neither — an older encoding, or bytes this build cannot read — refuses the whole log.
fn decode(seq: u64, bytes: &[u8]) -> Result<Record, ReadError> {
    let undecodable = |error: serde_json::Error| ReadError::Undecodable {
        seq,
        detail: error.to_string(),
    };
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
        _ => Err(ReadError::Undecodable {
            seq,
            detail: "it is neither an engine batch nor a host record".to_string(),
        }),
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
    contiguous_from(0, rows)
}

/// The batches of rows read in `seq` order from position `first`, refused unless their
/// positions run `first`, `first + 1`, …
pub(crate) fn contiguous_from(first: u64, rows: Vec<(i64, Vec<u8>)>) -> Result<Vec<Vec<u8>>, SequenceGap> {
    rows.into_iter()
        .zip(first..)
        .map(
            |((found, batch), expected)| match u64::try_from(found) == Ok(expected) {
                true => Ok(batch),
                false => Err(SequenceGap { expected, found }),
            },
        )
        .collect()
}

/// The policy file key a log's opening names. A log with no batch is an unknown root.
pub(crate) fn opening_key(root: &TrajectoryId, batches: &[Vec<u8>]) -> Result<PolicyFileKey, ReadError> {
    let Some(first) = batches.first() else {
        return Err(ReadError::UnknownRoot {
            root: root.as_str().to_string(),
        });
    };
    Ok(opening(root, first)?.1.policy_file_key)
}

/// A log's opening batch, and the opening record it begins with. An opening that names no
/// deployment is a family opened before policy revisions, which no engine of this build judges:
/// it is refused as [`ReadError::LegacyFamily`], not as bytes this build cannot read.
fn opening(root: &TrajectoryId, bytes: &[u8]) -> Result<(Record, TrajectoryOpening), ReadError> {
    let record = decode(0, bytes).map_err(|error| match legacy_opening(bytes) {
        true => ReadError::LegacyFamily {
            root: root.as_str().to_string(),
        },
        false => error,
    })?;
    match record.facts.first() {
        Some(Fact::TrajectoryOpened(opening)) => {
            let opening = opening.clone();
            Ok((record, opening))
        }
        _ => Err(ReadError::Undecodable {
            seq: 0,
            detail: "the log does not open with a TrajectoryOpened record".to_string(),
        }),
    }
}

/// Whether `bytes` hold an opening record that names no deployment.
fn legacy_opening(bytes: &[u8]) -> bool {
    let Ok(batch) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let facts = match &batch {
        serde_json::Value::Array(facts) => facts,
        serde_json::Value::Object(row) => match row.get("facts") {
            Some(serde_json::Value::Array(facts)) => facts,
            _ => return false,
        },
        _ => return false,
    };
    facts
        .first()
        .and_then(|fact| fact.get("TrajectoryOpened"))
        .and_then(serde_json::Value::as_object)
        .is_some_and(|opening| !opening.contains_key("deployment"))
}

/// The policy revision that decided a batch, where its first record carries one: the opening's
/// revision, or the stamp on the `BasisAdvanced` record every decided batch begins with. `None`
/// for a host-only batch.
pub(crate) fn stamp_of(facts: &[Fact]) -> Option<PolicyRevision> {
    match facts.first()? {
        Fact::TrajectoryOpened(TrajectoryOpening { revision, .. }) => Some(*revision),
        Fact::BasisAdvanced { revision, .. } => *revision,
        _ => None,
    }
}

pub(crate) fn decoded(root: &TrajectoryId, batches: Vec<Vec<u8>>, policy_file: Vec<u8>) -> Result<Log, ReadError> {
    let mut batches = batches.into_iter();
    let Some(first) = batches.next() else {
        return Err(ReadError::UnknownRoot {
            root: root.as_str().to_string(),
        });
    };
    let (record, opening) = opening(root, &first)?;
    let mut log = Log {
        root: root.clone(),
        deployment: opening.deployment,
        facts: Vec::new(),
        batch_ends: Vec::new(),
        stamps: Vec::new(),
        basis: 0,
        policy_file,
        host: Vec::new(),
    };
    push(&mut log, record);
    extend(&mut log, batches.collect())?;
    Ok(log)
}

/// Decode the batches that follow `log` in its root's log onto it, in order. Only the new
/// batches are decoded: a batch never changes once it is accepted.
pub(crate) fn extend(log: &mut Log, batches: Vec<Vec<u8>>) -> Result<(), ReadError> {
    for batch in batches {
        let record = decode(log.basis, &batch)?;
        push(log, record);
    }
    Ok(())
}

/// Fold one decoded batch onto `log`.
fn push(log: &mut Log, record: Record) {
    log.stamps.push(stamp_of(&record.facts));
    log.facts.extend(record.facts);
    log.batch_ends.push(log.facts.len());
    if let Some(observation) = record.host {
        log.host.push(HostRecord {
            seq: log.basis,
            observation,
        });
    }
    log.basis += 1;
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use appa_engine::value::DispatchId;
    use appa_runtime_api::Ruling;

    use appa_engine::label::{Audience, Label, Trust};
    use appa_engine::registry::TrustChain;

    use super::*;
    use crate::{
        AheadAttempt, AheadOutcome, DeclaredArguments, EndKind, HostActor, InvocationNonce, LabelSpelling, SpentVouch,
        StoredLabel,
    };
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
                prompt: Some("prompt-1".to_string()),
                ahead: None,
            },
            HostObservation::CallSettled {
                trajectory: root(),
                call_id: "toolu_1".to_string(),
            },
            HostObservation::CallDenied {
                trajectory: root(),
                call_id: "toolu_2".to_string(),
                batch: "cd".repeat(32),
                ahead: None,
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
            HostObservation::TurnEnded {
                actor: actor.clone(),
                end_kind: EndKind::Boundary,
            },
            HostObservation::AheadDeclared {
                actor,
                attempt: AheadAttempt::McpCall("toolu_3".to_string()),
                arguments: DeclaredArguments {
                    label: Some(LabelSpelling {
                        trust: Some("trusted".to_string()),
                        audience: None,
                    }),
                    return_schema: Some(serde_json::json!({ "type": "string" })),
                },
                label: stored_label(),
                spends: Some(SpentVouch {
                    key: "call:one".to_string(),
                }),
            },
            HostObservation::AheadReserved {
                trajectory: root(),
                attempt: AheadAttempt::Vouch(4),
                invocation: InvocationNonce(0xabc),
            },
            HostObservation::AheadSettled {
                trajectory: root(),
                attempt: AheadAttempt::Host("remedy:op".to_string()),
                outcome: AheadOutcome::Applied,
            },
        ]
    }

    fn stored_label() -> StoredLabel {
        StoredLabel::named(
            &Label::new(Trust::new(1), Audience::public()),
            &TrustChain::new(vec!["suspicious".to_string(), "trusted".to_string()]),
        )
    }

    /// The stored batch bytes are a persisted format: every host observation kind, the bare
    /// fact array, and an observation with no facts, byte for byte.
    #[test]
    fn stored_batch_bytes_are_frozen() {
        let facts = r#"[{"Boundary":{"trajectory":"cc:root","kind":"VoidReturn"}}]"#;
        let hosts = [
            r#"{"kind":"inventory","actor":"cc:root","adapter":"kagent","inventory":{"tools":[{"name":"read","tool":"mcp:demo/read"}],"sources":[]}}"#,
            r#"{"kind":"call_bound","trajectory":"cc:root","call_id":"toolu_1","dispatch":{"trajectory":"cc:root","digest":"abababababababababababababababababababababababababababababababab","occurrence":7},"prompt":"prompt-1"}"#,
            r#"{"kind":"call_settled","trajectory":"cc:root","call_id":"toolu_1"}"#,
            r#"{"kind":"call_denied","trajectory":"cc:root","call_id":"toolu_2","batch":"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"}"#,
            r#"{"kind":"vouched","actor":{"root":"cc:root","child":"cc:child"},"key":"offer:one","ruling":"approve"}"#,
            r#"{"kind":"claimed","actor":{"root":"cc:root","child":"cc:child"},"key":"offer:one","until":{"secs_since_epoch":1700000000,"nanos_since_epoch":5}}"#,
            r#"{"kind":"released","actor":{"root":"cc:root","child":"cc:child"},"key":"offer:one"}"#,
            r#"{"kind":"prompt_seen","actor":{"root":"cc:root","child":"cc:child"}}"#,
            r#"{"kind":"prompt_settled","actor":{"root":"cc:root","child":null}}"#,
            r#"{"kind":"turn_ended","actor":{"root":"cc:root","child":"cc:child"}}"#,
            r#"{"kind":"ahead_declared","actor":{"root":"cc:root","child":"cc:child"},"attempt":{"mcp_call":"toolu_3"},"arguments":{"label":{"trust":"trusted","audience":null},"return_schema":{"type":"string"}},"label":LABEL,"spends":{"key":"call:one"}}"#,
            r#"{"kind":"ahead_reserved","trajectory":"cc:root","attempt":{"vouch":4},"invocation":"00000000000000000000000000000abc"}"#,
            r#"{"kind":"ahead_settled","trajectory":"cc:root","attempt":{"host":"remedy:op"},"outcome":"applied"}"#,
        ];
        let observations = golden_observations();
        assert_eq!(observations.len(), hosts.len());

        let bare = encode(&punctuation(), None);
        assert_eq!(String::from_utf8(bare.clone()).unwrap(), facts);
        assert!(matches!(decode(0, &bare), Ok(Record { facts, host: None }) if facts == punctuation()));

        let label = serde_json::to_string(&stored_label()).expect("a label serializes");
        for (observation, host) in observations.iter().zip(hosts) {
            let host = host.replace("LABEL", &label);
            let host = host.as_str();
            let with_facts = encode(&punctuation(), Some(observation));
            assert_eq!(
                String::from_utf8(with_facts.clone()).unwrap(),
                format!(r#"{{"facts":{facts},"host":{host}}}"#)
            );
            assert!(matches!(
                decode(0, &with_facts),
                Ok(Record { facts, host: Some(decoded) }) if facts == punctuation() && &decoded == observation
            ));
            let alone = encode(&[], Some(observation));
            assert_eq!(
                String::from_utf8(alone.clone()).unwrap(),
                format!(r#"{{"facts":[],"host":{host}}}"#)
            );
            assert!(matches!(
                decode(0, &alone),
                Ok(Record { facts, host: Some(decoded) }) if facts.is_empty() && &decoded == observation
            ));
        }
    }

    /// The spellings the frozen goldens leave at their defaults: a housekeeping end, a first
    /// record that took a declaration, a declaration that spent no vouch, and every attempt and
    /// outcome. Each round-trips byte for byte.
    #[test]
    fn ahead_spellings_round_trip() {
        let actor = HostActor {
            root: root(),
            child: None,
        };
        let dispatch = DispatchId::new(
            root(),
            serde_json::from_value(serde_json::json!("ab".repeat(32))).expect("a digest decodes"),
            7,
        );
        let label = serde_json::to_string(&stored_label()).expect("a label serializes");
        let mut cases = vec![
            (
                HostObservation::TurnEnded {
                    actor: actor.clone(),
                    end_kind: EndKind::Housekeeping,
                },
                r#"{"kind":"turn_ended","actor":{"root":"cc:root","child":null},"end_kind":"housekeeping"}"#.to_string(),
            ),
            (
                HostObservation::CallBound {
                    trajectory: root(),
                    call_id: "toolu_1".to_string(),
                    dispatch,
                    prompt: None,
                    ahead: Some(AheadAttempt::McpCall("toolu_0".to_string())),
                },
                format!(
                    r#"{{"kind":"call_bound","trajectory":"cc:root","call_id":"toolu_1","dispatch":{{"trajectory":"cc:root","digest":"{}","occurrence":7}},"ahead":{{"mcp_call":"toolu_0"}}}}"#,
                    "ab".repeat(32)
                ),
            ),
            (
                HostObservation::CallDenied {
                    trajectory: root(),
                    call_id: "toolu_2".to_string(),
                    batch: "cd".to_string(),
                    ahead: Some(AheadAttempt::Host("remedy:op".to_string())),
                },
                r#"{"kind":"call_denied","trajectory":"cc:root","call_id":"toolu_2","batch":"cd","ahead":{"host":"remedy:op"}}"#
                    .to_string(),
            ),
            (
                HostObservation::AheadDeclared {
                    actor,
                    attempt: AheadAttempt::Vouch(9),
                    arguments: DeclaredArguments::default(),
                    label: stored_label(),
                    spends: None,
                },
                format!(
                    r#"{{"kind":"ahead_declared","actor":{{"root":"cc:root","child":null}},"attempt":{{"vouch":9}},"arguments":{{"label":null,"return_schema":null}},"label":{label}}}"#
                ),
            ),
        ];
        for (outcome, spelled) in [
            (AheadOutcome::Applied, "applied"),
            (AheadOutcome::FanOut, "fan_out"),
            (AheadOutcome::Lapsed, "lapsed"),
            (AheadOutcome::Unfit, "unfit"),
            (AheadOutcome::Ended, "ended"),
        ] {
            cases.push((
                HostObservation::AheadSettled {
                    trajectory: root(),
                    attempt: AheadAttempt::Vouch(1),
                    outcome,
                },
                format!(
                    r#"{{"kind":"ahead_settled","trajectory":"cc:root","attempt":{{"vouch":1}},"outcome":"{spelled}"}}"#
                ),
            ));
        }
        for (observation, host) in cases {
            let alone = encode(&[], Some(&observation));
            assert_eq!(
                String::from_utf8(alone.clone()).unwrap(),
                format!(r#"{{"facts":[],"host":{host}}}"#)
            );
            assert!(matches!(
                decode(0, &alone),
                Ok(Record { host: Some(decoded), .. }) if decoded == observation
            ));
        }
    }

    /// A declaration names the key a retry of its attempt finds it by, and the records that
    /// take or settle one name none.
    #[test]
    fn a_declaration_names_its_attempts_key() {
        for (attempt, key) in [
            (AheadAttempt::Host("remedy:op".to_string()), "ahead-attempt:host:remedy:op"),
            (AheadAttempt::McpCall("toolu_3".to_string()), "ahead-attempt:mcp:toolu_3"),
            (AheadAttempt::Vouch(4), "ahead-attempt:vouch:4"),
        ] {
            let declared = HostObservation::AheadDeclared {
                actor: HostActor {
                    root: root(),
                    child: None,
                },
                attempt: attempt.clone(),
                arguments: DeclaredArguments::default(),
                label: stored_label(),
                spends: None,
            };
            assert_eq!(declared.key().as_deref(), Some(key));
            let settled = HostObservation::AheadSettled {
                trajectory: root(),
                attempt: attempt.clone(),
                outcome: AheadOutcome::Ended,
            };
            let reserved = HostObservation::AheadReserved {
                trajectory: root(),
                attempt,
                invocation: InvocationNonce(1),
            };
            assert_eq!((settled.key(), reserved.key()), (None, None));
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
            decode(0, format!(" \n\t{facts}").as_bytes()),
            Ok(Record { facts, host: None }) if facts == punctuation()
        ));
        assert!(matches!(
            decode(0, format!("\r\n {{\"host\":{host}}}").as_bytes()),
            Ok(Record { facts, host: Some(decoded) }) if facts.is_empty() && decoded == seen
        ));
        for row in [b"".as_slice(), b"   ", br#""text""#, b"42", b"null"] {
            assert!(
                matches!(decode(3, row), Err(ReadError::Undecodable { seq: 3, .. })),
                "{}",
                String::from_utf8_lossy(row)
            );
        }
    }

    /// A batch that does not decode is refused under its own position in the root's log.
    #[test]
    fn an_undecodable_batch_is_refused_at_its_position() {
        let batches = vec![
            encode(&crate::tests::opening(&root()), None),
            encode(&punctuation(), None),
            b"not json at all".to_vec(),
        ];
        assert!(matches!(
            decoded(&root(), batches, Vec::new()),
            Err(ReadError::Undecodable { seq: 2, .. })
        ));
    }
}
