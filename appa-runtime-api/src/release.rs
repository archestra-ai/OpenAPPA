//! Receipts for content a trajectory released to its host, and the refusal a release judgment
//! answers with.

use sha2::{Digest as _, Sha256};

use crate::{Outcomes, RetryHint, TrajectoryId};

/// The SHA-256 of the bytes a receipt stands for: an admitted value's body, or a turn's final
/// answer. Spelled as 64 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReleaseDigest([u8; 32]);

impl ReleaseDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn parse(hex: &str) -> Option<Self> {
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut digest = [0u8; 32];
        for (slot, [high, low]) in digest.iter_mut().zip(bytes.as_chunks::<2>().0) {
            *slot = (digit(*high)? << 4) | digit(*low)?;
        }
        Some(Self(digest))
    }
}

impl std::fmt::Display for ReleaseDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

impl serde::Serialize for ReleaseDigest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for ReleaseDigest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        Self::parse(&hex).ok_or_else(|| serde::de::Error::custom("a release digest is 64 lowercase hex digits"))
    }
}

/// Where the runtime recorded one released content component: the family log of `session`,
/// the record at `fact_position` in it, and the digest of the bytes that record stands for.
/// It carries no label: a release judgment reads the label from that record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ReleaseReceipt {
    pub session: TrajectoryId,
    pub fact_position: u64,
    pub digest: ReleaseDigest,
}

/// Why content may not reach a sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseRefusal {
    /// A receipt names no released record of its session, or one with other bytes; or the sink
    /// is not a call a release may judge.
    Integrity,
    /// The sink's requirements fail, and no stock `approve` authority covers them.
    HardBlock,
    /// A fact the judgment reads was not obtained: no receipt, or no membership answer.
    MissingFact(RetryHint),
}

impl From<ReleaseRefusal> for Outcomes {
    fn from(refusal: ReleaseRefusal) -> Outcomes {
        match refusal {
            ReleaseRefusal::Integrity => Outcomes::Integrity,
            ReleaseRefusal::HardBlock => Outcomes::HardBlock,
            ReleaseRefusal::MissingFact(retry) => Outcomes::MissingFact(retry),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receipt_round_trips_with_its_digest_as_lowercase_hex() {
        let receipt = ReleaseReceipt {
            session: TrajectoryId::new("cc:s1"),
            fact_position: 7,
            digest: ReleaseDigest::of(b"done"),
        };
        let json = serde_json::to_value(&receipt).expect("serializes");
        assert_eq!(json["digest"], ReleaseDigest::of(b"done").to_string());
        assert_eq!(serde_json::from_value::<ReleaseReceipt>(json).expect("reads"), receipt);
        for hex in ["", "AB".repeat(32).as_str(), "zz".repeat(32).as_str()] {
            assert!(ReleaseDigest::parse(hex).is_none(), "{hex:?} must not parse");
        }
    }

    #[test]
    fn a_receipt_rides_only_on_a_decision_that_delivers() {
        use crate::{HookDecision, WireDecision};
        let receipt = ReleaseReceipt {
            session: TrajectoryId::new("cc:s1"),
            fact_position: 7,
            digest: ReleaseDigest::of(b"done"),
        };
        let bare = serde_json::to_value(WireDecision::of(&HookDecision::Ack)).expect("serializes");
        assert!(bare.get("receipt").is_none(), "an unset receipt is omitted");

        let acked = serde_json::to_value(WireDecision::of(&HookDecision::Ack).with_receipt(Some(receipt.clone())))
            .expect("serializes");
        let read: WireDecision = serde_json::from_value(acked).expect("reads");
        assert_eq!(read.receipt, Some(receipt.clone()));
        assert_eq!(read.into_decision(), Ok(HookDecision::Ack));

        let deferred = WireDecision::of(&HookDecision::DeferReturn).with_receipt(Some(receipt));
        assert!(deferred.into_decision().is_err());
    }
}
