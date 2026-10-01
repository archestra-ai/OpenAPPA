//! The vocabulary of one protected session messaging another: the address a session is
//! reachable at, the title its host shows, and the frame a delivered message arrives in.

use sha2::{Digest, Sha256};

/// The schemes a peer address may name: a Unix domain socket path, or a Windows named pipe.
const SCHEMES: [&str; 2] = ["uds:", "pipe:"];

const TITLE_MAX_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerValueError {
    AddressScheme { address: String },
    AddressEmpty { address: String },
    ControlCharacter,
    TitleEmpty,
    TitleLength { chars: usize },
    Digest { digest: String },
}

impl std::fmt::Display for PeerValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AddressScheme { address } => {
                write!(f, "{address:?}: a peer address starts with uds: or pipe:")
            }
            Self::AddressEmpty { address } => write!(f, "{address:?}: a peer address names a path after its scheme"),
            Self::ControlCharacter => f.write_str("a peer value carries a control character"),
            Self::TitleEmpty => f.write_str("a session title is empty"),
            Self::TitleLength { chars } => {
                write!(f, "a session title of {chars} characters; at most {TITLE_MAX_CHARS}")
            }
            Self::Digest { digest } => write!(f, "{digest:?}: a peer digest is 64 lowercase hex digits"),
        }
    }
}

impl std::error::Error for PeerValueError {}

/// Where one protected session receives peer messages: `uds:<path>` or `pipe:<name>`, as
/// its launcher bound it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerAddress(String);

impl PeerAddress {
    pub fn parse(text: &str) -> Result<Self, PeerValueError> {
        if text.chars().any(char::is_control) {
            return Err(PeerValueError::ControlCharacter);
        }
        let rest = SCHEMES
            .iter()
            .find_map(|scheme| text.strip_prefix(scheme))
            .ok_or_else(|| PeerValueError::AddressScheme {
                address: text.to_string(),
            })?;
        match rest.is_empty() {
            true => Err(PeerValueError::AddressEmpty {
                address: text.to_string(),
            }),
            false => Ok(Self(text.to_string())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PeerAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The title a host shows for one session, trimmed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionTitle(String);

impl SessionTitle {
    pub fn parse(text: &str) -> Result<Self, PeerValueError> {
        let title = text.trim();
        if title.is_empty() {
            return Err(PeerValueError::TitleEmpty);
        }
        if title.chars().any(char::is_control) {
            return Err(PeerValueError::ControlCharacter);
        }
        match title.chars().count() {
            chars if chars > TITLE_MAX_CHARS => Err(PeerValueError::TitleLength { chars }),
            _ => Ok(Self(title.to_string())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionTitle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The SHA-256 of one peer message's body. The sender's side digests the message it asked
/// to send and the receiver's side digests the body its frame delivered, through the same
/// [`PeerDigest::of_body`], so equal digests are the same bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerDigest([u8; 32]);

impl PeerDigest {
    pub fn of_body(body: &str) -> Self {
        Self(Sha256::digest(body.as_bytes()).into())
    }

    pub fn parse(hex: &str) -> Result<Self, PeerValueError> {
        let refuse = || PeerValueError::Digest {
            digest: hex.to_string(),
        };
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return Err(refuse());
        }
        let mut digest = [0u8; 32];
        for (slot, [high, low]) in digest.iter_mut().zip(bytes.as_chunks::<2>().0) {
            *slot = (digit(*high).ok_or_else(refuse)? << 4) | digit(*low).ok_or_else(refuse)?;
        }
        Ok(Self(digest))
    }
}

impl std::fmt::Display for PeerDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// A prompt that arrived as another session's message. `Parsed` names the sender's address
/// and the digest of the body delivered; `Malformed` is a prompt spelled as a peer frame
/// whose sender or body could not be read, so it is never mistaken for a person's prompt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "SpelledFrame", into = "SpelledFrame")]
pub enum PeerFrame {
    Parsed { from: PeerAddress, digest: PeerDigest },
    Malformed,
}

/// [`PeerFrame`] as stored and posted. Serde ignores `deny_unknown_fields` on a unit variant
/// of an internally tagged enum, so `Malformed` is spelled as an empty struct here to refuse
/// a malformed frame that carries a sender.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case", deny_unknown_fields)]
enum SpelledFrame {
    Parsed { from: PeerAddress, digest: PeerDigest },
    Malformed {},
}

impl From<SpelledFrame> for PeerFrame {
    fn from(frame: SpelledFrame) -> Self {
        match frame {
            SpelledFrame::Parsed { from, digest } => Self::Parsed { from, digest },
            SpelledFrame::Malformed {} => Self::Malformed,
        }
    }
}

impl From<PeerFrame> for SpelledFrame {
    fn from(frame: PeerFrame) -> Self {
        match frame {
            PeerFrame::Parsed { from, digest } => Self::Parsed { from, digest },
            PeerFrame::Malformed => Self::Malformed {},
        }
    }
}

macro_rules! serde_as_string {
    ($name:ident) => {
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                $name::parse(&text).map_err(serde::de::Error::custom)
            }
        }
    };
}

serde_as_string!(PeerAddress);
serde_as_string!(SessionTitle);
serde_as_string!(PeerDigest);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_names_a_known_scheme_and_a_path() {
        for address in ["uds:/tmp/appa-peer-probe/a.sock", "pipe:\\\\.\\pipe\\appa-a"] {
            assert_eq!(
                PeerAddress::parse(address).map(|a| a.to_string()),
                Ok(address.to_string())
            );
        }
        for address in [
            "",
            "uds:",
            "pipe:",
            "tcp://127.0.0.1:1",
            "/tmp/a.sock",
            "uds:/tmp/a\n.sock",
        ] {
            assert!(PeerAddress::parse(address).is_err(), "{address:?} must not parse");
        }
    }

    #[test]
    fn a_title_is_trimmed_bounded_and_printable() {
        assert_eq!(
            SessionTitle::parse("  peer-b \n").map(|t| t.to_string()),
            Ok("peer-b".to_string())
        );
        assert_eq!(SessionTitle::parse(&"t".repeat(200)).map(|t| t.as_str().len()), Ok(200));
        assert_eq!(
            SessionTitle::parse(&"t".repeat(201)),
            Err(PeerValueError::TitleLength { chars: 201 })
        );
        assert_eq!(SessionTitle::parse(" \t"), Err(PeerValueError::TitleEmpty));
        assert_eq!(SessionTitle::parse("a\u{7}b"), Err(PeerValueError::ControlCharacter));
    }

    #[test]
    fn a_digest_is_the_sha256_of_the_body_and_round_trips_as_hex() {
        let digest = PeerDigest::of_body("abc");
        assert_eq!(
            digest.to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(PeerDigest::parse(&digest.to_string()), Ok(digest));
        for hex in [
            "",
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            &"g".repeat(64),
        ] {
            assert!(PeerDigest::parse(hex).is_err(), "{hex:?} must not parse");
        }
    }

    #[test]
    fn a_frame_serializes_under_its_tag() {
        let parsed = PeerFrame::Parsed {
            from: PeerAddress::parse("uds:/a.sock").expect("parses"),
            digest: PeerDigest::of_body("abc"),
        };
        for frame in [parsed, PeerFrame::Malformed] {
            let json = serde_json::to_string(&frame).expect("serializes");
            assert_eq!(serde_json::from_str::<PeerFrame>(&json).expect("deserializes"), frame);
        }
        assert_eq!(
            serde_json::to_string(&PeerFrame::Malformed).expect("serializes"),
            r#"{"frame":"malformed"}"#
        );
        for refused in [
            r#"{"frame":"malformed","from":"uds:/a.sock"}"#,
            r#"{"frame":"parsed","from":"tcp:/a","digest":"00"}"#,
            r#"{"frame":"other"}"#,
        ] {
            assert!(serde_json::from_str::<PeerFrame>(refused).is_err(), "{refused}");
        }
    }
}
