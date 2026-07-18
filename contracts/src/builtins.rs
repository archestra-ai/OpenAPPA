//! Compiled-in transformer implementations the TOML dialect can bind by name
//! (`builtin = "redact-email"`). TOML cannot carry code, so a declaration
//! picks one of these; the declaration's `output` label — the operator's
//! trust decision — is what the engine enforces, never the implementation's
//! cleverness ("admitted under the transition declared by registered
//! transformer X", not "verified as clean").

use appa_core::{OpaqueValue, TransformerError, TransformerFn};

/// Every builtin name the dialect accepts, for the load-error message.
pub(crate) const KNOWN_NAMES: &[&str] = &["redact-email"];

pub(crate) fn resolve(name: &str) -> Option<TransformerFn> {
    match name {
        "redact-email" => Some(redact_email),
        _ => None,
    }
}

const REPLACEMENT: &str = "[redacted-email]";

/// Replace email addresses inside every string value of a JSON document.
///
/// JSON-aware on purpose: the body is a tool call's arguments JSON, and a
/// raw byte scan would miss escape-equivalent addresses (`"alice@x.io"`
/// decodes to an email when the harness parses the arguments). The document
/// is decoded, each string *value* redacted (object keys are left alone —
/// they are schema, not payload), and reserialized only when something was
/// redacted, so an address-free body round-trips byte-identical. A non-JSON
/// body is a `TransformerError` — fail closed, no half-scanned admission.
fn redact_email(body: &OpaqueValue) -> Result<OpaqueValue, TransformerError> {
    let mut doc: serde_json::Value = serde_json::from_str(body.as_str()).map_err(|e| TransformerError {
        message: format!("redact-email: body is not JSON: {e}"),
    })?;
    if redact_strings(&mut doc) == 0 {
        return Ok(body.clone());
    }
    let out = serde_json::to_string(&doc).expect("a decoded JSON document reserializes");
    Ok(OpaqueValue::new(out))
}

fn redact_strings(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(s) => {
            let (redacted, count) = redact_emails_in(s);
            if count > 0 {
                *s = redacted;
            }
            count
        }
        serde_json::Value::Array(items) => items.iter_mut().map(redact_strings).sum(),
        serde_json::Value::Object(map) => map.values_mut().map(redact_strings).sum(),
        _ => 0,
    }
}

/// The accepted email grammar, over the decoded string: a maximal
/// `local@domain` token where `local` is one or more of `[A-Za-z0-9._%+-]`
/// and `domain` is dot-separated labels of `[A-Za-z0-9-]` (no leading or
/// trailing hyphen), at least two labels, the last purely alphabetic and at
/// least two characters. Deliberately conservative: a token that fails the
/// grammar is left untouched rather than guessed at.
fn redact_emails_in(s: &str) -> (String, usize) {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut count = 0;
    let mut emitted = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let start = local_start(bytes, i);
            let end = domain_end(bytes, i);
            if start < i && end > i + 1 && valid_domain(&s[i + 1..end]) {
                out.push_str(&s[emitted..start]);
                out.push_str(REPLACEMENT);
                count += 1;
                emitted = end;
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[emitted..]);
    (out, count)
}

fn is_local_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_domain_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

/// Byte index where the local part starts: expand left from the `@`.
/// All member bytes are ASCII, so the boundary always lands between chars.
fn local_start(bytes: &[u8], at: usize) -> usize {
    let mut start = at;
    while start > 0 && is_local_byte(bytes[start - 1]) {
        start -= 1;
    }
    start
}

/// Byte index one past the domain: expand right from the `@`, then trim
/// trailing punctuation so `alice@example.com.` keeps its sentence period.
fn domain_end(bytes: &[u8], at: usize) -> usize {
    let mut end = at + 1;
    while end < bytes.len() && is_domain_byte(bytes[end]) {
        end += 1;
    }
    while end > at + 1 && matches!(bytes[end - 1], b'.' | b'-') {
        end -= 1;
    }
    end
}

fn valid_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let all_wellformed = labels
        .iter()
        .all(|l| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-'));
    let tld = labels.last().expect("split yields at least one label");
    all_wellformed && tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::{Value, json};

    use super::*;

    fn redact(body: &str) -> Result<String, TransformerError> {
        redact_email(&OpaqueValue::new(body)).map(|v| v.as_str().to_string())
    }

    #[test]
    fn redacts_addresses_in_string_values() {
        let out = redact(r#"{"message":"checkout failing for alice.smith+test@example.com, restarting"}"#).unwrap();
        assert_eq!(
            out,
            r#"{"message":"checkout failing for [redacted-email], restarting"}"#
        );
    }

    #[test]
    fn redacts_in_nested_structures_and_counts_every_hit() {
        let out = redact(r#"{"a":["bob@x.io","clean"],"b":{"c":"two: a@y.dev b@z.org"}}"#).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["a"][0], "[redacted-email]");
        assert_eq!(doc["a"][1], "clean");
        assert_eq!(doc["b"]["c"], "two: [redacted-email] [redacted-email]");
    }

    #[test]
    fn decodes_unicode_escaped_at_sign() {
        // @ is `@` once decoded — the reason redaction is JSON-aware.
        let out = redact(r#"{"m":"contact alice@example.com now"}"#).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["m"], "contact [redacted-email] now");
    }

    #[test]
    fn address_at_string_boundaries_and_sentence_period() {
        let doc: Value = serde_json::from_str(&redact(r#"{"m":"alice@example.com"}"#).unwrap()).unwrap();
        assert_eq!(doc["m"], "[redacted-email]");
        let doc: Value = serde_json::from_str(&redact(r#"{"m":"mail alice@example.com."}"#).unwrap()).unwrap();
        assert_eq!(doc["m"], "mail [redacted-email].");
    }

    #[test]
    fn non_addresses_are_left_alone() {
        for body in [
            r#"{"m":"@handle mentions stay"}"#,
            r#"{"m":"a@@b.com is not an address"}"#,
            r#"{"m":"no-tld a@b"}"#,
            r#"{"m":"numeric tld a@b.12"}"#,
            r#"{"m":"v1@2 not an address"}"#,
        ] {
            assert_eq!(redact(body).unwrap(), body, "body `{body}` should be untouched");
        }
    }

    #[test]
    fn object_keys_are_not_redacted() {
        let body = r#"{"alice@example.com":"clean"}"#;
        assert_eq!(redact(body).unwrap(), body);
    }

    #[test]
    fn non_json_body_is_a_transformer_error() {
        assert!(redact_email(&OpaqueValue::new("not json")).is_err());
        assert!(redact_email(&OpaqueValue::new("")).is_err());
    }

    #[test]
    fn unknown_builtin_resolves_to_none() {
        assert!(resolve("redact-email").is_some());
        assert!(resolve("redact-ssn").is_none());
    }

    /// Arbitrary JSON documents whose strings may contain address-like and
    /// address-adjacent text.
    fn arb_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|n| json!(n)),
            "[a-zA-Z0-9 @._%+-]{0,40}".prop_map(Value::String),
        ];
        leaf.prop_recursive(3, 24, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::btree_map("[a-z]{1,6}", inner, 0..4)
                    .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #[test]
        fn law_deterministic(doc in arb_json()) {
            let body = doc.to_string();
            prop_assert_eq!(redact(&body).unwrap(), redact(&body).unwrap());
        }

        #[test]
        fn law_idempotent(doc in arb_json()) {
            let once = redact(&doc.to_string()).unwrap();
            prop_assert_eq!(redact(&once).unwrap(), once.clone());
        }

        #[test]
        fn law_output_is_valid_json_with_structure_preserved(doc in arb_json()) {
            let out: Value = serde_json::from_str(&redact(&doc.to_string()).unwrap()).unwrap();
            prop_assert_eq!(shape(&out), shape(&doc));
        }

        #[test]
        fn law_address_free_body_roundtrips_byte_identical(doc in arb_json()) {
            let body = doc.to_string();
            prop_assume!(!body.contains('@'));
            prop_assert_eq!(redact(&body).unwrap(), body);
        }
    }

    /// The non-string skeleton of a document: everything redaction must
    /// preserve.
    fn shape(value: &Value) -> Value {
        match value {
            Value::String(_) => Value::String(String::new()),
            Value::Array(items) => Value::Array(items.iter().map(shape).collect()),
            Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
            other => other.clone(),
        }
    }
}
