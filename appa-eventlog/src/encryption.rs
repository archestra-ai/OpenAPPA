//! Request-scoped encryption for an embedded host's retained trajectory content.
//! A view never falls back to plaintext when its key is absent or incorrect.
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use serde_json::{Value, json};
use std::sync::Arc;

const MAGIC: &[u8] = b"\0appa-storage-v1\0";
const TEXT: &str = "appa-storage-v1:";
const JSON_KEY: &str = "__appa_encrypted";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StorageEncryptionError(&'static str);

impl From<StorageEncryptionError> for crate::postgres::PostgresError {
    fn from(error: StorageEncryptionError) -> Self {
        Self(error.to_string())
    }
}

impl From<StorageEncryptionError> for crate::receipts::ReceiptError {
    fn from(error: StorageEncryptionError) -> Self {
        Self::storage(error.to_string())
    }
}

/// Unambiguous column/row binding, also used by embedding hosts for their SQL.
pub fn field_context(parts: &[&str]) -> String {
    parts.iter().map(|part| format!("{}:{part}", part.len())).collect()
}

/// A host constructs an encrypted view for one authenticated request. Cloning
/// shares only that request's key; no process-wide key registry is involved.
#[derive(Clone, Default)]
pub enum StorageCipher {
    #[default]
    Plain,
    Encrypted(Arc<EncryptionKey>),
}

pub struct EncryptionKey {
    key: aead::LessSafeKey,
    scope: String,
}

impl StorageCipher {
    pub fn encrypted(key: &[u8], scope: String) -> Result<Self, StorageEncryptionError> {
        if key.len() != 32 || scope.is_empty() || scope.len() > 2048 || scope.chars().any(char::is_control) {
            return Err(StorageEncryptionError("invalid storage encryption context"));
        }
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, key)
            .map_err(|_| StorageEncryptionError("invalid storage encryption key"))?;
        Ok(Self::Encrypted(Arc::new(EncryptionKey {
            key: aead::LessSafeKey::new(key),
            scope,
        })))
    }

    pub fn is_encrypted(&self) -> bool {
        matches!(self, Self::Encrypted(_))
    }

    pub fn seal_bytes(&self, context: &str, bytes: &[u8]) -> Result<Vec<u8>, StorageEncryptionError> {
        let Self::Encrypted(material) = self else {
            return Ok(if bytes.starts_with(MAGIC) {
                [MAGIC, &[0], bytes].concat()
            } else {
                bytes.to_vec()
            });
        };
        let mut nonce = [0; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| StorageEncryptionError("storage nonce generation failed"))?;
        let mut output = bytes.to_vec();
        material
            .key
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aad(material, context),
                &mut output,
            )
            .map_err(|_| StorageEncryptionError("storage encryption failed"))?;
        Ok([MAGIC, &[1], &nonce, &output].concat())
    }

    pub fn open_bytes(&self, context: &str, bytes: &[u8]) -> Result<Vec<u8>, StorageEncryptionError> {
        let Some(body) = bytes.strip_prefix(MAGIC) else {
            return match self {
                Self::Plain => Ok(bytes.to_vec()),
                Self::Encrypted(_) => Err(StorageEncryptionError("encrypted storage contains plaintext")),
            };
        };
        if body.first() == Some(&0) && matches!(self, Self::Plain) {
            return Ok(body[1..].to_vec());
        }
        let Self::Encrypted(material) = self else {
            return Err(StorageEncryptionError(
                "encrypted storage requires its conversation key",
            ));
        };
        if body.first() != Some(&1) || body.len() < 1 + 12 + 16 {
            return Err(StorageEncryptionError("invalid encrypted storage envelope"));
        }
        let nonce = aead::Nonce::try_assume_unique_for_key(&body[1..13])
            .map_err(|_| StorageEncryptionError("invalid encrypted storage nonce"))?;
        let mut ciphertext = body[13..].to_vec();
        material
            .key
            .open_in_place(nonce, aad(material, context), &mut ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|_| StorageEncryptionError("encrypted storage authentication failed"))
    }

    pub fn seal_json(&self, context: &str, value: &Value) -> Result<Value, StorageEncryptionError> {
        if !self.is_encrypted() && !json_envelope(value) {
            return Ok(value.clone());
        }
        let raw = serde_json::to_vec(value).map_err(|_| StorageEncryptionError("storage JSON encoding failed"))?;
        let bytes = if self.is_encrypted() {
            self.seal_bytes(context, &raw)?
        } else {
            [MAGIC, &[0], &raw].concat()
        };
        Ok(json!({JSON_KEY: STANDARD.encode(bytes)}))
    }

    pub fn open_json(&self, context: &str, value: Value) -> Result<Value, StorageEncryptionError> {
        if !json_envelope(&value) {
            return if self.is_encrypted() {
                Err(StorageEncryptionError("encrypted storage contains plaintext JSON"))
            } else {
                Ok(value)
            };
        }
        let encoded = value[JSON_KEY]
            .as_str()
            .ok_or(StorageEncryptionError("invalid encrypted storage JSON envelope"))?;
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| StorageEncryptionError("invalid encrypted storage JSON encoding"))?;
        serde_json::from_slice(&self.open_bytes(context, &bytes)?)
            .map_err(|_| StorageEncryptionError("storage JSON decoding failed"))
    }

    pub fn seal_text(&self, context: &str, text: &str) -> Result<String, StorageEncryptionError> {
        if !self.is_encrypted() && !text.starts_with(TEXT) {
            return Ok(text.to_owned());
        }
        let bytes = if self.is_encrypted() {
            self.seal_bytes(context, text.as_bytes())?
        } else {
            [MAGIC, &[0], text.as_bytes()].concat()
        };
        Ok(format!("{TEXT}{}", STANDARD.encode(bytes)))
    }

    pub fn open_text(&self, context: &str, text: String) -> Result<String, StorageEncryptionError> {
        // The root, not arbitrary tool output, identifies the storage mode.
        // Plain readers retain literal envelope-like text and also accept the
        // explicit plaintext escape that existing storage writers produce.
        if !self.is_encrypted() {
            if let Some(encoded) = text.strip_prefix(TEXT)
                && let Ok(bytes) = STANDARD.decode(encoded)
                && let Some(literal) = bytes.strip_prefix(MAGIC).and_then(|body| body.strip_prefix(&[0]))
                && let Ok(literal) = std::str::from_utf8(literal)
            {
                return Ok(literal.to_owned());
            }
            return Ok(text);
        }
        let Some(encoded) = text.strip_prefix(TEXT) else {
            return Err(StorageEncryptionError("encrypted storage contains plaintext text"));
        };
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| StorageEncryptionError("invalid encrypted storage text envelope"))?;
        String::from_utf8(self.open_bytes(context, &bytes)?)
            .map_err(|_| StorageEncryptionError("storage text decoding failed"))
    }
}

fn aad(material: &EncryptionKey, context: &str) -> aead::Aad<Vec<u8>> {
    aead::Aad::from(format!("appa-storage-v1\0{}\0{context}", material.scope).into_bytes())
}

fn json_envelope(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.len() == 1 && object.contains_key(JSON_KEY))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ciphertext_needs_the_same_key_scope_and_column() {
        let cipher = StorageCipher::encrypted(&[7; 32], "conversation-a".into()).unwrap();
        let bytes = cipher.seal_bytes("events:root:0", b"private tool argument").unwrap();
        assert!(!bytes.windows(7).any(|part| part == b"private"));
        assert_eq!(
            cipher.open_bytes("events:root:0", &bytes).unwrap(),
            b"private tool argument"
        );
        assert!(StorageCipher::Plain.open_bytes("events:root:0", &bytes).is_err());
        assert!(
            StorageCipher::encrypted(&[8; 32], "conversation-a".into())
                .unwrap()
                .open_bytes("events:root:0", &bytes)
                .is_err()
        );
        assert!(
            StorageCipher::encrypted(&[7; 32], "conversation-b".into())
                .unwrap()
                .open_bytes("events:root:0", &bytes)
                .is_err()
        );
        assert!(cipher.open_bytes("events:root:1", &bytes).is_err());
        let mut tampered = bytes;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(cipher.open_bytes("events:root:0", &tampered).is_err());
        assert!(cipher.open_bytes("events:root:0", b"plaintext").is_err());
    }
    #[test]
    fn json_text_and_marker_like_plaintext_round_trip() {
        for cipher in [
            StorageCipher::Plain,
            StorageCipher::encrypted(&[1; 32], "conversation".into()).unwrap(),
        ] {
            for value in [
                json!({"argument":"private"}),
                json!({JSON_KEY:"user content"}),
                Value::Null,
            ] {
                let stored = cipher.seal_json("operation", &value).unwrap();
                assert_eq!(cipher.open_json("operation", stored).unwrap(), value);
            }
            for text in ["result", "appa-storage-v1:user content"] {
                assert_eq!(
                    cipher
                        .open_text("result", cipher.seal_text("result", text).unwrap())
                        .unwrap(),
                    text
                );
            }
        }
    }
}
