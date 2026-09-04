//! The classification walk: what of a fact may leave this machine.
//!
//! # Why a walk and not a projection
//!
//! The obvious way to build a diagnostic is to write a struct holding the fields worth
//! reporting and fill it in. That fails open. A new field on an existing engine type is
//! invisible to it: the projection keeps compiling, keeps producing the same shape, and the
//! new field is simply absent — which is safe — until someone extends the projection and
//! forgets one. Nothing tells them.
//!
//! So instead the fact is serialized and *walked* against a table, and the table is
//! **deny-by-default and total**:
//!
//! - A key the table does not name keeps its place and loses its contents entirely. The
//!   whole value, however deeply nested, becomes `"<unclassified>"` and its path is
//!   recorded. A reader learns that an unknown field exists and where — the part worth
//!   knowing — without a byte of it leaving.
//! - An aggregate whose type has no table is *not* serialized whole. It takes the same
//!   `"<unclassified>"` path. Forgetting a table is therefore a failing fixture test naming
//!   the path that needs one, never a silent leak.
//!
//! # The table is written against the serialized shape
//!
//! Several engine types bend their JSON away from their Rust struct: `ReturnShape` and
//! `CanonicalArguments` have hand-written `Serialize` impls, `GroupRef`, `ChainAudience` and
//! `ValueBody` serialize as bare strings, and `PinnedAnnotation`, `EffectSet`, `Audience` and
//! `ToolDeclarationId` are `#[serde(transparent)]` so their wrapper level does not exist in
//! the JSON at all. Every entry below describes the emitted JSON, and the fixtures compare
//! against real serialized output rather than a hand-written idea of it.
//!
//! # Names hide in four kinds of place
//!
//! As values, as the *keys* of typed maps, as the *elements* of sets that serialize to
//! arrays, and packed inside a single scalar string. A rule exists for each, because a rule
//! for only the first would ship the other three.

use serde_json::{Map, Value};

use super::tokens::{Class, Mode, Tokens};

/// What is left where the table did not classify something.
pub(crate) const UNCLASSIFIED: &str = "<unclassified>";

/// The placeholder a recorded path uses in place of a dynamic key.
///
/// A path is reported so a reader can see *where* drift is. If it spelled the key it found,
/// the very list that reports drift would leak the deployment-defined name it drifted on.
const DYNAMIC_SEGMENT: &str = "{key}";

/// How one JSON value is carried.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Rule {
    /// A scalar with no name in it: a number, a boolean, a content digest, a closed-set string
    /// the engine itself defines. Never use this for anything a deployment spells.
    Keep,
    /// A scalar that names no person but does identify the deployment — the policy digest and
    /// the stored policy's key. Carried as it stands in [`Mode::Baseline`], dropped under
    /// pseudonymization, where the whole point is that two reports cannot be tied together.
    Fingerprint,
    /// A deployment-defined name, carried through the report's substitution table.
    Token(Class),
    /// A value body: replaced by its length, so a reader sees that something large crossed
    /// without seeing any of it.
    BodyBytes,
    /// Tool-call arguments: the *keys* survive, every value is dropped. Which arguments a
    /// call carried is diagnostic; what was in them is the caller's data.
    ArgumentKeys,
    /// An array whose items are names. A set of `ToolName` serializes to an array, and no
    /// map-key rule reaches it.
    Elements(Class),
    /// An array whose items each follow one rule.
    Each(&'static Rule),
    /// A map whose *keys* are deployment-defined names, and whose values follow their own
    /// rule.
    MapKeys(Class, &'static Rule),
    /// A scalar string that packs names rather than holding one: `GroupRef`, which
    /// serializes as `"@group"` or `"@provider:selector"`.
    PackedName,
    /// A return contract, which serializes as a JSON Schema document. Authored text hides in
    /// its property names at every depth, its `required` array, its `const`/`enum` literals,
    /// and its `description` prose.
    ReturnSchema,
    /// Recurse into a named table.
    Table(&'static Table),
}

/// One aggregate's classification: every key its JSON may carry.
#[derive(Debug)]
pub(crate) struct Table {
    pub(crate) name: &'static str,
    pub(crate) entries: &'static [(&'static str, Rule)],
}

impl Rule {
    /// The key this rule emits under, where the fact's own key would misdescribe what is left.
    /// `arguments` becomes `argument_keys` and a body becomes `body_bytes`, so a reader is never
    /// shown an array of names under a key that promises values.
    fn emitted_key<'k>(&self, key: &'k str) -> &'k str {
        match self {
            Rule::ArgumentKeys => "argument_keys",
            Rule::BodyBytes => "body_bytes",
            _ => key,
        }
    }

    /// Whether this rule carries nothing at all in this mode. An omitted key is absent from the
    /// object rather than present and empty; in an array, where there is no key to leave out,
    /// the same rule yields `null`.
    fn omitted(&self, mode: Mode) -> bool {
        matches!(self, Rule::Fingerprint) && mode == Mode::Pseudonymized
    }
}

impl Table {
    fn rule(&self, key: &str) -> Option<&'static Rule> {
        self.entries.iter().find(|(name, _)| *name == key).map(|(_, rule)| rule)
    }
}

/// One place the inventory did not cover.
///
/// Both fields are the walk's own vocabulary — a key path it built and a table name from this
/// crate — never a key or a value read from the input, so the drift report cannot itself carry
/// data. `table` is what makes the report actionable: it names the aggregate whose entry list
/// needs the missing line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Drift {
    pub(crate) path: String,
    pub(crate) table: &'static str,
}

/// What the walk produced, and what it could not classify.
#[derive(Debug, Default)]
pub(crate) struct Stripped {
    pub(crate) value: Value,
    pub(crate) unclassified: Vec<Drift>,
}

/// The walk's running state.
struct Walk<'a> {
    tokens: &'a mut Tokens,
    mode: Mode,
    /// The table currently being walked, so a drift entry names the aggregate and not only
    /// the path.
    table: &'static str,
    unclassified: Vec<Drift>,
}

impl Walk<'_> {
    fn unclassify(&mut self, path: &str) -> Value {
        let drift = Drift {
            path: path.to_string(),
            table: self.table,
        };
        if !self.unclassified.contains(&drift) {
            self.unclassified.push(drift);
        }
        Value::String(UNCLASSIFIED.to_string())
    }

    fn token(&mut self, class: Class, raw: &str) -> Value {
        Value::String(self.tokens.token(self.mode, class, raw))
    }

    /// Object keys in lexicographic order, array elements in index order.
    ///
    /// Sorted here rather than inherited from `serde_json::Map`: token numbers follow first
    /// appearance, and that contract must not depend on whether some crate in the workspace
    /// turns on `serde_json/preserve_order`.
    fn sorted(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
        let mut pairs: Vec<(&String, &Value)> = map.iter().collect();
        pairs.sort_by_key(|(left, _)| *left);
        pairs
    }

    fn apply(&mut self, rule: &'static Rule, value: &Value, path: &str) -> Value {
        // An absent optional holds nothing, so no rule needs to look inside it. Without this
        // every `Option<T>` field would need a rule of its own saying the same thing.
        if value.is_null() {
            return Value::Null;
        }
        match rule {
            Rule::Keep => match value {
                // `Keep` promises "no name in here". An aggregate cannot make that promise,
                // so one that reaches it is a table error, not a licence.
                Value::Object(_) | Value::Array(_) => self.unclassify(path),
                scalar => scalar.clone(),
            },
            Rule::Fingerprint => match self.mode {
                Mode::Baseline => self.apply(&Rule::Keep, value, path),
                Mode::Pseudonymized => Value::Null,
            },
            Rule::Token(class) => match value.as_str() {
                Some(raw) => self.token(*class, raw),
                None => self.unclassify(path),
            },
            Rule::BodyBytes => match value.as_str() {
                Some(body) => Value::from(body.len()),
                None => self.unclassify(path),
            },
            Rule::ArgumentKeys => self.argument_keys(value, path),
            Rule::Elements(class) => match value.as_array() {
                Some(items) => Value::Array(
                    items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| match item.as_str() {
                            Some(raw) => self.token(*class, raw),
                            None => self.unclassify(&format!("{path}[{index}]")),
                        })
                        .collect(),
                ),
                None => self.unclassify(path),
            },
            Rule::Each(inner) => match value.as_array() {
                Some(items) => Value::Array(
                    items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| self.apply(inner, item, &format!("{path}[{index}]")))
                        .collect(),
                ),
                None => self.unclassify(path),
            },
            Rule::MapKeys(class, inner) => match value.as_object() {
                Some(map) => {
                    let mut out = Map::new();
                    for (key, entry) in Self::sorted(map) {
                        // The path names a placeholder, never the key itself.
                        let child = format!("{path}.{DYNAMIC_SEGMENT}");
                        let tokenized = self.tokens.token(self.mode, *class, key);
                        let value = self.apply(inner, entry, &child);
                        out.insert(tokenized, value);
                    }
                    Value::Object(out)
                }
                None => self.unclassify(path),
            },
            Rule::PackedName => self.packed_name(value, path),
            Rule::ReturnSchema => self.return_schema(value, path),
            Rule::Table(table) => self.table(table, value, path),
        }
    }

    /// Which arguments a call carried, never what was in them.
    fn argument_keys(&mut self, value: &Value, path: &str) -> Value {
        // `CanonicalArguments` serializes as a scalar string holding canonical JSON, not as
        // an object: a rule written against the Rust struct would not match this at all.
        let parsed = match value {
            Value::String(text) => serde_json::from_str::<Value>(text).ok(),
            other => Some(other.clone()),
        };
        match parsed.as_ref().and_then(Value::as_object) {
            Some(map) => Value::Array(
                Self::sorted(map)
                    .into_iter()
                    .map(|(key, _)| Value::from(key.as_str()))
                    .collect(),
            ),
            None => self.unclassify(path),
        }
    }

    /// `GroupRef`, which is always `@`-marked: `"@group"`, or `"@provider:selector"` split at
    /// the *first* colon, since a selector may itself contain `@` and `.`.
    fn packed_name(&mut self, value: &Value, path: &str) -> Value {
        let Some(spelled) = value.as_str() else {
            return self.unclassify(path);
        };
        let Some(after_at) = spelled.strip_prefix('@') else {
            return self.unclassify(path);
        };
        match after_at.split_once(':') {
            None if after_at.is_empty() => self.unclassify(path),
            None => {
                let group = self.tokens.token(self.mode, Class::Group, after_at);
                Value::String(format!("@{group}"))
            }
            // An empty half either side is malformed: the rule cannot know what it holds.
            Some((provider, selector)) if provider.is_empty() || selector.is_empty() => self.unclassify(path),
            Some((provider, selector)) => {
                let provider = self.tokens.token(self.mode, Class::Source, provider);
                let selector = self.tokens.token(self.mode, Class::Selector, selector);
                Value::String(format!("@{provider}:{selector}"))
            }
        }
    }

    /// A return contract's JSON Schema document.
    ///
    /// Re-normalized rather than merely re-serialized: `ReturnShape` refuses any document
    /// that is not byte-identical to its own normalization, which sorts string-enum members
    /// and rebuilds `required` from sorted property names. Emitting tokens in allocation
    /// order would produce `["literal-1", "literal-10", "literal-2"]` out of order and a
    /// report the engine cannot read back.
    fn return_schema(&mut self, value: &Value, path: &str) -> Value {
        let Some(node) = value.as_object() else {
            return self.unclassify(path);
        };
        let mut out = Map::new();
        for (key, entry) in Self::sorted(node) {
            match key.as_str() {
                // Free prose authored into a contract: no diagnostic value, no bound on what
                // it might say.
                "description" => {}
                "properties" => match entry.as_object() {
                    Some(fields) => {
                        let mut renamed = Map::new();
                        for (name, shape) in Self::sorted(fields) {
                            let token = self.tokens.token(self.mode, Class::Field, name);
                            let child = format!("{path}.properties.{DYNAMIC_SEGMENT}");
                            renamed.insert(token, self.return_schema(shape, &child));
                        }
                        out.insert(key.clone(), Value::Object(renamed));
                    }
                    None => {
                        let child = format!("{path}.properties");
                        let placeholder = self.unclassify(&child);
                        out.insert(key.clone(), placeholder);
                    }
                },
                // Authored names as array *items*: no map-key rule reaches these.
                "required" => {
                    let mut names: Vec<String> = entry
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(|name| self.tokens.token(self.mode, Class::Field, name))
                                .collect()
                        })
                        .unwrap_or_default();
                    names.sort_unstable();
                    out.insert(key.clone(), Value::Array(names.into_iter().map(Value::from).collect()));
                }
                "const" => match entry.as_str() {
                    Some(literal) => {
                        let token = self.tokens.token(self.mode, Class::Literal, literal);
                        out.insert(key.clone(), Value::String(token));
                    }
                    None => {
                        let child = format!("{path}.const");
                        let placeholder = self.unclassify(&child);
                        out.insert(key.clone(), placeholder);
                    }
                },
                "enum" => {
                    let mut members: Vec<String> = entry
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(|literal| self.tokens.token(self.mode, Class::Literal, literal))
                                .collect()
                        })
                        .unwrap_or_default();
                    members.sort_unstable();
                    out.insert(
                        key.clone(),
                        Value::Array(members.into_iter().map(Value::from).collect()),
                    );
                }
                "items" => {
                    let child = format!("{path}.items");
                    let nested = self.return_schema(entry, &child);
                    out.insert(key.clone(), nested);
                }
                // `type`, `format` (a closed set), and the numeric bounds: engine vocabulary,
                // not the author's.
                "type"
                | "format"
                | "minimum"
                | "maximum"
                | "multipleOf"
                | "minItems"
                | "maxItems"
                | "additionalProperties" => {
                    out.insert(key.clone(), entry.clone());
                }
                _ => {
                    let child = format!("{path}.{key}");
                    let placeholder = self.unclassify(&child);
                    out.insert(key.clone(), placeholder);
                }
            }
        }
        Value::Object(out)
    }

    fn table(&mut self, table: &'static Table, value: &Value, path: &str) -> Value {
        // An externally tagged enum's *unit* variant serializes as a bare string rather than a
        // one-key object, so a table for such an enum must name its unit variants too. They are
        // engine vocabulary, so `Keep` is the only rule that fits one; anything else in that
        // slot, or a variant the table does not name, is drift.
        let outer = std::mem::replace(&mut self.table, table.name);
        let stripped = self.table_entries(table, value, path);
        self.table = outer;
        stripped
    }

    fn table_entries(&mut self, table: &'static Table, value: &Value, path: &str) -> Value {
        if let Value::String(variant) = value {
            return match table.rule(variant) {
                Some(Rule::Keep) => value.clone(),
                _ => self.unclassify(path),
            };
        }
        let Some(map) = value.as_object() else {
            return self.unclassify(path);
        };
        let mut out = Map::new();
        for (key, entry) in Self::sorted(map) {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            match table.rule(key) {
                Some(rule) if rule.omitted(self.mode) => {}
                Some(rule) => {
                    let value = self.apply(rule, entry, &child);
                    out.insert(rule.emitted_key(key).to_string(), value);
                }
                // The table does not name this key: it keeps its place and loses everything
                // it held, at any depth.
                None => {
                    let placeholder = self.unclassify(&child);
                    out.insert(key.clone(), placeholder);
                }
            }
        }
        Value::Object(out)
    }
}

/// Walk one serialized value against a table.
pub(crate) fn strip(value: &Value, table: &'static Table, tokens: &mut Tokens, mode: Mode) -> Stripped {
    let mut walk = Walk {
        tokens,
        mode,
        table: table.name,
        unclassified: Vec::new(),
    };
    let stripped = walk.table(table, value, "");
    Stripped {
        value: stripped,
        unclassified: walk.unclassified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static NESTED: Table = Table {
        name: "Nested",
        entries: &[("tool", Rule::Token(Class::Tool))],
    };

    static ROOT: Table = Table {
        name: "Root",
        entries: &[
            ("tool", Rule::Token(Class::Tool)),
            ("count", Rule::Keep),
            ("body", Rule::BodyBytes),
            ("arguments", Rule::ArgumentKeys),
            ("confined", Rule::Elements(Class::Tool)),
            ("exceptions", Rule::MapKeys(Class::Tool, &Rule::Keep)),
            ("group", Rule::PackedName),
            ("shape", Rule::ReturnSchema),
            ("nested", Rule::Table(&NESTED)),
            ("fingerprint", Rule::Fingerprint),
        ],
    };

    fn run(value: serde_json::Value) -> Stripped {
        let mut tokens = Tokens::default();
        strip(&value, &ROOT, &mut tokens, Mode::Pseudonymized)
    }

    fn drift(stripped: &Stripped) -> Vec<(&str, &str)> {
        stripped
            .unclassified
            .iter()
            .map(|drift| (drift.path.as_str(), drift.table))
            .collect()
    }

    /// The property the whole module exists for: a field nobody classified keeps its place
    /// and loses everything, at any depth.
    #[test]
    fn an_unknown_key_loses_its_whole_value_however_deep() {
        let stripped = run(serde_json::json!({
            "surprise": { "deep": { "deeper": "s3cret-value" } }
        }));
        assert_eq!(stripped.value["surprise"], UNCLASSIFIED);
        // The drift entry names the aggregate whose table needs the missing line, not only
        // where the value sat.
        assert_eq!(drift(&stripped), vec![("surprise", "Root")]);
        let rendered = serde_json::to_string(&stripped.value).expect("the stripped value serializes");
        assert!(!rendered.contains("s3cret-value"), "no substring of the value survives");
        assert!(!rendered.contains("deeper"), "not even its nested keys survive");
    }

    /// A deployment fingerprint is diagnostic in a baseline report and is exactly what two
    /// pseudonymized reports must not share.
    #[test]
    fn a_fingerprint_survives_the_baseline_and_is_gone_under_pseudonymization() {
        let value = serde_json::json!({ "fingerprint": "a91f", "count": 7 });
        let mut tokens = Tokens::default();
        let baseline = strip(&value, &ROOT, &mut tokens, Mode::Baseline);
        assert_eq!(baseline.value["fingerprint"], "a91f");

        let pseudonymized = run(value);
        assert!(pseudonymized.value.get("fingerprint").is_none());
        assert_eq!(pseudonymized.value["count"], 7);
        assert!(pseudonymized.unclassified.is_empty(), "an omission is not drift");
    }

    #[test]
    fn a_body_becomes_its_length_under_a_key_that_says_so() {
        let stripped = run(serde_json::json!({ "body": "abcde" }));
        assert!(stripped.value.get("body").is_none(), "the key is renamed, not kept");
        assert_eq!(stripped.value["body_bytes"], 5);
    }

    #[test]
    fn arguments_keep_their_keys_and_lose_their_values() {
        let stripped = run(serde_json::json!({
            "arguments": r#"{"file_path":"/home/someone/.ssh/id_rsa","limit":10}"#
        }));
        assert!(
            stripped.value.get("arguments").is_none(),
            "the key is renamed, not kept"
        );
        assert_eq!(
            stripped.value["argument_keys"],
            serde_json::json!(["file_path", "limit"])
        );
        let rendered = serde_json::to_string(&stripped.value).expect("serializes");
        assert!(!rendered.contains("id_rsa"));
    }

    /// A `BTreeSet<ToolName>` serializes to an array, so no map-key rule reaches it.
    #[test]
    fn set_elements_are_tokenized() {
        let stripped = run(serde_json::json!({ "confined": ["Bash", "Read"] }));
        assert_eq!(stripped.value["confined"], serde_json::json!(["tool-1", "tool-2"]));
    }

    #[test]
    fn map_keys_are_tokenized_and_their_paths_name_a_placeholder() {
        let stripped = run(serde_json::json!({ "exceptions": { "Bash": true } }));
        let exceptions = stripped.value["exceptions"].as_object().expect("an object");
        assert_eq!(exceptions.keys().collect::<Vec<_>>(), vec!["tool-1"]);
        assert!(!stripped.unclassified.iter().any(|drift| drift.path.contains("Bash")));
    }

    #[test]
    fn both_group_shapes_are_split_and_reassembled() {
        let named = run(serde_json::json!({ "group": "@finance" }));
        assert_eq!(named.value["group"], "@group-1");
        let sourced = run(serde_json::json!({ "group": "@google-workspace:group/finance@corp.com" }));
        assert_eq!(sourced.value["group"], "@source-1:selector-1");
        let rendered = serde_json::to_string(&sourced.value).expect("serializes");
        assert!(!rendered.contains("corp.com"), "the selector's own spelling is gone");
    }

    /// An unparseable spelling is exactly the case where the rule cannot know what it holds.
    #[test]
    fn a_malformed_group_takes_the_unclassified_path() {
        let stripped = run(serde_json::json!({ "group": "finance" }));
        assert_eq!(stripped.value["group"], UNCLASSIFIED);
        assert_eq!(drift(&stripped), vec![("group", "Root")]);
    }

    #[test]
    fn a_return_schema_tokenizes_names_and_literals_and_drops_prose() {
        let stripped = run(serde_json::json!({
            "shape": {
                "type": "object",
                "description": "an internal note nobody meant to publish",
                "properties": {
                    "verdict": { "type": "string", "enum": ["approved", "denied"] },
                    "who": { "type": "string", "const": "finance-team" }
                },
                "required": ["verdict", "who"]
            }
        }));
        let shape = &stripped.value["shape"];
        assert!(shape.get("description").is_none(), "authored prose is dropped");
        assert_eq!(shape["type"], "object");
        let properties = shape["properties"].as_object().expect("an object");
        let mut names: Vec<&String> = properties.keys().collect();
        names.sort();
        assert_eq!(names, vec!["field-1", "field-2"]);
        let rendered = serde_json::to_string(shape).expect("serializes");
        for authored in ["verdict", "who", "approved", "denied", "finance-team", "nobody meant"] {
            assert!(!rendered.contains(authored), "{authored} still appears in {rendered}");
        }
    }

    /// Ten or more members is where a naive `-N` spelling stops sorting the way the engine's
    /// own normalization does.
    #[test]
    fn schema_arrays_come_back_sorted_so_the_shape_still_normalizes() {
        let members: Vec<String> = (0..12).map(|index| format!("member-{index:02}")).collect();
        let stripped = run(serde_json::json!({
            "shape": { "type": "string", "enum": members }
        }));
        let emitted: Vec<&str> = stripped.value["shape"]["enum"]
            .as_array()
            .expect("an array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mut sorted = emitted.clone();
        sorted.sort_unstable();
        assert_eq!(emitted, sorted, "a normalized schema's enum is sorted");
        assert_eq!(emitted.len(), 12);
    }

    /// `Keep` promises there is no name inside. An aggregate cannot make that promise.
    #[test]
    fn keep_refuses_an_aggregate_rather_than_passing_it_through() {
        let stripped = run(serde_json::json!({ "count": { "smuggled": "a-name" } }));
        assert_eq!(stripped.value["count"], UNCLASSIFIED);
        assert_eq!(drift(&stripped), vec![("count", "Root")]);
    }

    #[test]
    fn a_nested_table_is_walked_and_its_paths_are_qualified() {
        let stripped = run(serde_json::json!({ "nested": { "tool": "Read", "extra": "leak-me" } }));
        assert_eq!(stripped.value["nested"]["tool"], "tool-1");
        assert_eq!(stripped.value["nested"]["extra"], UNCLASSIFIED);
        assert_eq!(drift(&stripped), vec![("nested.extra", "Nested")]);
    }

    /// The walk sorts keys itself, so token numbers cannot move if some crate in the
    /// workspace turns on `serde_json/preserve_order`.
    #[test]
    fn token_numbering_follows_sorted_key_order_not_insertion_order() {
        let first = run(serde_json::json!({ "nested": { "tool": "Zebra" }, "tool": "Apple" }));
        let second = run(serde_json::json!({ "tool": "Apple", "nested": { "tool": "Zebra" } }));
        assert_eq!(
            first.value["nested"]["tool"], "tool-1",
            "\"nested\" sorts before \"tool\""
        );
        assert_eq!(first.value["tool"], "tool-2");
        assert_eq!(first.value, second.value, "insertion order does not matter");
    }
}
