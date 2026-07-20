//! Compiled-in transformer implementations the TOML dialect can bind by name
//! (`builtin = "redact-email"`). TOML cannot carry code, so a declaration
//! picks one of these; the declaration's `output` label — the operator's
//! trust decision — is what the engine enforces, never the implementation's
//! cleverness ("admitted under the transition declared by registered
//! transformer X", not "verified as clean").

use appa_core::{OpaqueValue, TransformerError, TransformerFn};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

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
/// redacted, so an address-free body round-trips byte-identical. When a
/// redaction does occur, reserialization normalizes the rest of the
/// document too — whitespace, number spelling, escape forms — while member
/// order is preserved (see Cargo.toml's `preserve_order` note). A non-JSON
/// body is a `TransformerError` — fail closed, no half-scanned admission.
/// So is a document with duplicate object keys: parsing would collapse them,
/// and a first-wins downstream parser could read an occurrence this walk
/// never saw — refuse rather than redact half a document.
fn redact_email(body: &OpaqueValue) -> Result<OpaqueValue, TransformerError> {
    let mut deserializer = serde_json::Deserializer::from_str(body.as_str());
    let NoDupValue(mut doc) = NoDupValue::deserialize(&mut deserializer).map_err(|e| TransformerError {
        message: format!("redact-email: body is not JSON without duplicate keys: {e}"),
    })?;
    deserializer.end().map_err(|e| TransformerError {
        message: format!("redact-email: body is not a single JSON document: {e}"),
    })?;
    if redact_strings(&mut doc) == 0 {
        return Ok(body.clone());
    }
    let out = serde_json::to_string(&doc).expect("a decoded JSON document reserializes");
    Ok(OpaqueValue::new(out))
}

struct NoDupValue(serde_json::Value);

impl<'de> Deserialize<'de> for NoDupValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(NoDupVisitor)
    }
}

struct NoDupVisitor;

impl<'de> Visitor<'de> for NoDupVisitor {
    type Value = NoDupValue;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(NoDupValue(serde_json::Value::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(NoDupValue(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(NoDupValue(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
        Ok(NoDupValue(v.into()))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(NoDupValue(serde_json::Value::String(v.to_string())))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(NoDupValue(serde_json::Value::String(v)))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(NoDupValue(serde_json::Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(NoDupValue(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(NoDupValue(serde_json::Value::Array(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut object = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(serde::de::Error::custom(format!("duplicate object key `{key}`")));
            }
            let NoDupValue(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(NoDupValue(serde_json::Value::Object(object)))
    }
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
            // Clamp the local part at the consumed boundary: text an earlier
            // match already replaced behaves like any non-local character
            // (exactly what the replacement token's `]` would be on a
            // rescan), so one pass reaches the fixpoint — no accepted
            // address survives and a second pass changes nothing
            // (`a@b.co_c@d.co` redacts twice in one pass; `a@b.co@d.co`'s
            // second candidate clamps to an empty local and is refused,
            // matching the rescan where `]@d.co` has no local either).
            let start = local_start(bytes, i).max(emitted);
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

fn local_start(bytes: &[u8], at: usize) -> usize {
    let mut start = at;
    while start > 0 && is_local_byte(bytes[start - 1]) {
        start -= 1;
    }
    start
}

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
    let mut labels = 0;
    let mut tld = "";
    for label in domain.split('.') {
        if label.is_empty() || label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        labels += 1;
        tld = label;
    }
    labels >= 2 && tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_alphabetic())
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
        let body = r#"{"m":"contact alice\u0040example.com now"}"#;
        assert!(!body.contains('@'), "the fixture must not contain a literal @");
        let doc: Value = serde_json::from_str(&redact(body).unwrap()).unwrap();
        assert_eq!(doc["m"], "contact [redacted-email] now");
    }

    #[test]
    fn overlapping_candidates_redact_the_first_and_skip_the_consumed_tail() {
        let out = redact(r#"{"m":"a@b.co@d.co"}"#).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["m"], "[redacted-email]@d.co");
    }

    #[test]
    fn a_clamped_local_part_still_redacts_the_second_address_in_one_pass() {
        let out = redact(r#"{"m":"a@b.co_c@d.co"}"#).unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["m"], "[redacted-email][redacted-email]");
        assert_eq!(redact(&out).unwrap(), out);
    }

    #[test]
    fn multibyte_neighbors_of_the_at_sign_never_match_or_panic() {
        for body in [
            r#"{"m":"é@example.com"}"#,
            r#"{"m":"a@éxample.com"}"#,
            r#"{"m":"héllo wörld @ é"}"#,
        ] {
            assert_eq!(redact(body).unwrap(), body, "body `{body}` should be untouched");
        }
    }

    #[test]
    fn long_tokens_do_not_blow_up() {
        let long = format!(r#"{{"m":"a@{}aa"}}"#, "a.".repeat(10_000));
        let out = redact(&long).unwrap();
        assert!(out.contains("[redacted-email]"));
    }

    #[test]
    fn duplicate_object_keys_are_refused_at_every_level() {
        for body in [
            r#"{"x":"alice@example.com","x":"clean"}"#,
            r#"{"outer":{"x":"alice@example.com","x":"clean"}}"#,
            r#"{"a":[{"x":1,"x":2}]}"#,
        ] {
            assert!(redact(body).is_err(), "body `{body}` should be refused");
        }
    }

    #[test]
    fn redaction_preserves_member_order_of_unrelated_keys() {
        let out = redact(r#"{"z":"alice@example.com","a":1}"#).unwrap();
        assert_eq!(out, r#"{"z":"[redacted-email]","a":1}"#);
    }

    #[test]
    fn trailing_garbage_after_the_document_is_refused() {
        assert!(redact(r#"{"m":"x"} trailing"#).is_err());
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

        #[test]
        fn law_never_panics_on_arbitrary_bytes(body in ".{0,200}") {
            let _ = redact_email(&OpaqueValue::new(body));
        }
    }

    fn shape(value: &Value) -> Value {
        match value {
            Value::String(_) => Value::String(String::new()),
            Value::Array(items) => Value::Array(items.iter().map(shape).collect()),
            Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
            other => other.clone(),
        }
    }
}
