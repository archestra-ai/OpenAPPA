//! The lowercase-hex wire form of the engine's 32-byte digests.
//!
//! JSON has no bytes type, so serde renders a `[u8; 32]` as a sequence of 32 decimal
//! integers. Every identity the engine derives is such a digest, the event log is JSON, and
//! the runtime already surfaces offers to the model as 64 hex characters. These functions
//! give every digest that one form: an order of magnitude smaller on the record, and a
//! string an operator can match against a hash printed anywhere else.

use serde::{Deserializer, Serializer, de};

/// The character count of a digest's hex form.
const HEX_LEN: usize = 64;

pub(crate) fn encode(bytes: &[u8; 32]) -> String {
    let mut hex = String::with_capacity(HEX_LEN);
    for byte in bytes {
        hex.push(hex_char(byte >> 4));
        hex.push(hex_char(byte & 0x0f));
    }
    hex
}

/// Parse a digest's hex form. Untrusted input: a wrong length, a non-hex character, or an
/// uppercase one is refused, never guessed.
pub(crate) fn decode(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != HEX_LEN {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, [high, low]) in bytes.as_chunks::<2>().0.iter().enumerate() {
        out[index] = (nibble(*high)? << 4) | nibble(*low)?;
    }
    Some(out)
}

fn hex_char(nibble: u8) -> char {
    char::from_digit(u32::from(nibble), 16).expect("a nibble is one hex digit")
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

pub(crate) fn serialize<S: Serializer>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&encode(bytes))
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
    deserializer.deserialize_str(HexDigest)
}

struct HexDigest;

impl de::Visitor<'_> for HexDigest {
    type Value = [u8; 32];

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("64 lowercase-hex characters")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<[u8; 32], E> {
        decode(text).ok_or_else(|| E::invalid_value(de::Unexpected::Str(text), &self))
    }
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn a_digest_round_trips_through_its_hex_form() {
        let mut bytes = [0u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(7).wrapping_add(3);
        }
        assert_eq!(decode(&encode(&bytes)), Some(bytes));
    }

    #[test]
    fn every_byte_renders_as_two_lowercase_digits() {
        assert_eq!(encode(&[0u8; 32]), "0".repeat(64));
        assert_eq!(encode(&[255u8; 32]), "ff".repeat(32));
    }

    #[test]
    fn malformed_hex_is_refused() {
        let valid = encode(&[0xabu8; 32]);
        assert_eq!(decode(&valid[..63]), None);
        assert_eq!(decode(&format!("{valid}0")), None);
        assert_eq!(decode(&valid.to_uppercase()), None);
        assert_eq!(decode(&format!("g{}", &valid[1..])), None);
    }
}
