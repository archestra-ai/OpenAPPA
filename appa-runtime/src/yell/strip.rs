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

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::tokens::{Class, Mode, Tokens};

/// What is left where the table did not classify something.
pub(crate) const UNCLASSIFIED: &str = "<unclassified>";

/// The placeholder a recorded path uses in place of a dynamic key.
///
/// A path is reported so a reader can see *where* drift is. If it spelled the key it found,
/// the very list that reports drift would leak the deployment-defined name it drifted on.
const DYNAMIC_SEGMENT: &str = "{key}";

/// The longest an unclassified key may be and still keep its spelling. Comfortably longer
/// than any field name in the workspace and far short of anything worth smuggling.
const MAX_KEY_BYTES: usize = 64;

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
    /// Carried in no mode, for a field that legitimately appears and whose content may never
    /// leave: a deployer's `description` and `hint`. Those are the deployment's own sentences,
    /// and a sentence is bounded by nothing — it holds a path, an endpoint or a colleague as
    /// readily as the reason a rule exists.
    ///
    /// This is for content that *is* expected. A field that cannot appear in valid input is
    /// left unnamed instead, so that one appearing raises drift, which is the only signal that
    /// something upstream changed.
    Never,
    /// A deployment-defined name, carried through the report's substitution table.
    Token(Class),
    /// A tool name, which is the deployment's vocabulary only when the policy writes it.
    ///
    /// Every other name in the inventory reaches a fact because a deployment wrote it in a
    /// policy. A tool name does not have to: the harness announces whatever tool the model
    /// asked for, and APPA records the hook whether or not that name is declared — which is
    /// the point, since "APPA refused a tool I never declared" is a common thing to report.
    /// So the spelling is checked against the policy before Baseline may carry it, and a
    /// name the policy does not write is tokenized in both modes.
    VouchedTool,
    /// A value body: replaced by its length, so a reader sees that something large crossed
    /// without seeing any of it.
    BodyBytes,
    /// Tool-call arguments: how many the call carried and which of them recur, as tokens.
    /// Every value is dropped, and so is every key's spelling — a key is the caller's data
    /// just as its value is, because an open-parameter tool accepts any object and the model
    /// chooses the names in it.
    ArgumentKeys,
    /// An array whose items are names. A set of `ToolName` serializes to an array, and no
    /// map-key rule reaches it.
    Elements(Class),
    /// An array whose items each follow one rule.
    Each(&'static Rule),
    /// One value or an array of them, for a field the schema leaves untagged over the two.
    /// `[deployment.starting_label] audience` is written either as the bare token `public` or
    /// as a list, and a rule that reads only the list refuses the commoner spelling.
    OneOrMany(&'static Rule),
    /// A map whose *keys* are deployment-defined names, and whose values follow their own
    /// rule.
    MapKeys(Class, &'static Rule),
    /// A scalar string that packs names rather than holding one: `GroupRef`, which
    /// serializes as `"@group"` or `"@provider:selector"`.
    PackedName,
    /// One entry of an audience list as a *policy* writes it, where four unlike things share
    /// one slot: the closed words `public`, `self` and `internal`; `@group` or
    /// `@provider:selector`; `$argument`, naming a tool argument to read recipients from; and
    /// anything else, which is a reader written out — a person. Each is carried as what it is,
    /// so a policy's commonest clause stays readable without a literal address riding along
    /// beside it.
    AudienceToken,
    /// `provider:selector`, as a policy writes an audience source. The provider is a stock
    /// name the engine defines. The selector is not: a source template offers
    /// `group/<group-address>`, and the loader accepts the instantiated
    /// `group/finance@corp.example` in its place, so the selector half is an address as often
    /// as it is a word — and the rule cannot tell which. It is a token in either mode.
    AudienceSource,
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
        match self {
            Rule::Never => true,
            Rule::Fingerprint => mode == Mode::Pseudonymized,
            _ => false,
        }
    }
}

impl Table {
    fn rule(&self, key: &str) -> Option<&'static Rule> {
        self.entries.iter().find(|(name, _)| *name == key).map(|(_, rule)| rule)
    }
}

/// One place the inventory did not cover.
///
/// `table` is a table name from this crate. `path` is built from table keys and from the
/// unclassified key itself, which is what makes a drift report actionable — it names the
/// aggregate and the field whose entry is missing. That key is admitted only when it is
/// identifier-shaped (see `Walk::shown_key`), so a path cannot carry a caller's spelling even
/// though it is assembled from input. No value ever reaches either field.
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
    /// The tool names the serving policy writes. Data, not a callback: the set is resolved
    /// once per export and read by [`Rule::VouchedTool`].
    vouched: &'a BTreeSet<String>,
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
            Rule::Never => Value::Null,
            Rule::Token(class) => match value.as_str() {
                Some(raw) => self.token(*class, raw),
                None => self.unclassify(path),
            },
            Rule::VouchedTool => match value.as_str() {
                Some(raw) if self.vouched.contains(raw) => self.token(Class::Tool, raw),
                Some(raw) => Value::String(self.tokens.token(Mode::Pseudonymized, Class::Tool, raw)),
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
            Rule::Each(inner) => self.each(inner, value, path),
            Rule::OneOrMany(inner) => match value.is_array() {
                true => self.each(inner, value, path),
                false => self.apply(inner, value, path),
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
            Rule::AudienceToken => match value.as_str() {
                // The engine's own words for the two ends of the audience chain and for the
                // universe. A deployment does not choose these spellings.
                Some("public" | "self" | "internal") => value.clone(),
                Some(group) if group.starts_with('@') => self.packed_name(value, path),
                Some(placeholder) => match placeholder.strip_prefix('$') {
                    Some("") | None => self.token(Class::Reader, placeholder),
                    Some(argument) => {
                        Value::String(format!("${}", self.tokens.token(self.mode, Class::Argument, argument)))
                    }
                },
                None => self.unclassify(path),
            },
            Rule::AudienceSource => match value.as_str() {
                Some(spelled) => self.source(spelled, path),
                None => self.unclassify(path),
            },
            Rule::ReturnSchema => self.return_schema(value, path),
            Rule::Table(table) => self.table(table, value, path),
        }
    }

    fn each(&mut self, inner: &'static Rule, value: &Value, path: &str) -> Value {
        match value.as_array() {
            Some(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| self.apply(inner, item, &format!("{path}[{index}]")))
                    .collect(),
            ),
            None => self.unclassify(path),
        }
    }

    /// How many arguments a call carried and which of them recur — never what was in them,
    /// and never how they were spelled.
    ///
    /// A key is tokenized like any other caller-chosen string. It is tempting to carry keys as
    /// spelled, since a declared tool's parameters are named in the policy; but nothing
    /// confines a call to declared names — an open-parameter tool takes any object — so a call
    /// carrying `{"/home/alice/.ssh/id_rsa": true}` would export that path *classified*, where
    /// no drift report would ever mention it.
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
                    .map(|(key, _)| self.token(Class::Argument, key))
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
        match after_at.contains(':') {
            false if after_at.is_empty() => self.unclassify(path),
            false => {
                let group = self.tokens.token(self.mode, Class::Group, after_at);
                Value::String(format!("@{group}"))
            }
            true => match self.source(after_at, path) {
                Value::String(source) => Value::String(format!("@{source}")),
                refused => refused,
            },
        }
    }

    /// `provider:selector`, split at the *first* colon since a selector may itself contain a
    /// colon, an `@` or a dot. The provider half is a stock name; the selector half is a token
    /// in either mode, because the loader accepts a concrete `group/finance@corp.example`
    /// wherever it accepts the template that address instantiates.
    fn source(&mut self, spelled: &str, path: &str) -> Value {
        match spelled.split_once(':') {
            // An empty half either side is malformed: the rule cannot know what it holds.
            Some((provider, selector)) if !provider.is_empty() && !selector.is_empty() => {
                let provider = self.tokens.token(self.mode, Class::Source, provider);
                let selector = self.tokens.token(self.mode, Class::Selector, selector);
                Value::String(format!("{provider}:{selector}"))
            }
            _ => self.unclassify(path),
        }
    }

    /// One authored literal from a return contract, as a token of its own spelling.
    ///
    /// Keyed on the literal's canonical JSON so that a number and the string of the same
    /// digits stay distinct, and so that two schemas naming the same bound share a token.
    fn literal(&mut self, value: &Value) -> Value {
        let spelling = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
        Value::String(self.tokens.token(self.mode, Class::Literal, &spelling))
    }

    /// The spelling a key may keep when the inventory does not name it.
    ///
    /// Every key that reaches this is a Rust field name or a serde variant name, because
    /// every map with caller-chosen keys has a rule of its own that tokenizes them. So a key
    /// that is not identifier-shaped did not come from a struct in this workspace, and its
    /// spelling is data: it loses its spelling as its value already loses its own. This keeps
    /// the drift report actionable — a new engine field appears under its real name — without
    /// letting the drift path become the leak it exists to prevent.
    fn shown_key(key: &str) -> String {
        let identifier =
            !key.is_empty() && key.len() <= MAX_KEY_BYTES && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        match identifier {
            true => key.to_string(),
            false => DYNAMIC_SEGMENT.to_string(),
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
                // Every literal the author wrote, of whatever JSON type. A number is as much
                // the agent's choice as a string is, and a 64-bit integer bound carries more
                // of a message than most strings would: `minimum` is a channel, not
                // arithmetic. So a literal becomes a token of its own spelling, which keeps
                // "these two schemas bound the same field the same way" legible and carries
                // none of the value.
                "const" => {
                    let token = self.literal(entry);
                    out.insert(key.clone(), token);
                }
                "enum" => match entry.as_array() {
                    Some(items) => {
                        let mut members: Vec<Value> = items.iter().map(|item| self.literal(item)).collect();
                        members.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
                        out.insert(key.clone(), Value::Array(members));
                    }
                    None => {
                        let child = format!("{path}.enum");
                        let placeholder = self.unclassify(&child);
                        out.insert(key.clone(), placeholder);
                    }
                },
                // The bounds, which the author also chose. Same reasoning as `const`: a length
                // or a pattern is a channel as much as a string is, and a regex is authored
                // text with nothing bounding what it can spell.
                "minimum" | "maximum" | "multipleOf" | "minItems" | "maxItems" | "minLength" | "maxLength"
                | "pattern" => {
                    let token = self.literal(entry);
                    out.insert(key.clone(), token);
                }
                "items" => {
                    let child = format!("{path}.items");
                    let nested = self.return_schema(entry, &child);
                    out.insert(key.clone(), nested);
                }
                // A closed schema is written `false`, which is the dialect's own word. An
                // object there is another schema, full of authored names, and is walked as one.
                "additionalProperties" => match entry.is_boolean() {
                    true => {
                        out.insert(key.clone(), entry.clone());
                    }
                    false => {
                        let child = format!("{path}.additionalProperties");
                        let nested = self.return_schema(entry, &child);
                        out.insert(key.clone(), nested);
                    }
                },
                // `type` and `format` are closed sets the dialect defines, so they are engine
                // vocabulary rather than the author's.
                "type" | "format" => {
                    out.insert(key.clone(), entry.clone());
                }
                _ => {
                    let shown = Self::shown_key(key);
                    let child = format!("{path}.{shown}");
                    let placeholder = self.unclassify(&child);
                    out.insert(shown, placeholder);
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
            let shown = Self::shown_key(key);
            let child = if path.is_empty() {
                shown.clone()
            } else {
                format!("{path}.{shown}")
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
                    out.insert(shown, placeholder);
                }
            }
        }
        Value::Object(out)
    }
}

/// Walk one serialized value against a table.
pub(crate) fn strip(
    value: &Value,
    table: &'static Table,
    tokens: &mut Tokens,
    mode: Mode,
    vouched: &BTreeSet<String>,
) -> Stripped {
    let mut walk = Walk {
        tokens,
        mode,
        vouched,
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
        entries: &[("tool", Rule::VouchedTool), ("digest", Rule::Token(Class::Digest))],
    };

    static ROOT: Table = Table {
        name: "Root",
        entries: &[
            ("tool", Rule::VouchedTool),
            ("reader", Rule::Token(Class::Reader)),
            ("session", Rule::Token(Class::Trajectory)),
            ("digest", Rule::Token(Class::Digest)),
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

    /// The tool names the fixture deployment writes, so a test can put a name the policy
    /// chose beside one the model invented.
    fn vouched() -> BTreeSet<String> {
        ["Bash", "Read"].into_iter().map(str::to_string).collect()
    }

    fn run(value: serde_json::Value) -> Stripped {
        let mut tokens = Tokens::default();
        strip(&value, &ROOT, &mut tokens, Mode::Pseudonymized, &vouched())
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
        let baseline = strip(&value, &ROOT, &mut tokens, Mode::Baseline, &vouched());
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
    fn arguments_keep_their_shape_and_lose_their_keys_and_values() {
        let stripped = run(serde_json::json!({
            "arguments": r#"{"file_path":"/home/someone/.ssh/id_rsa","limit":10}"#
        }));
        assert!(
            stripped.value.get("arguments").is_none(),
            "the key is renamed, not kept"
        );
        assert_eq!(
            stripped.value["argument_keys"],
            serde_json::json!(["argument-1", "argument-2"]),
            "how many arguments and which recur, never how they were spelled"
        );
        let rendered = serde_json::to_string(&stripped.value).expect("serializes");
        assert!(!rendered.contains("id_rsa"));
    }

    /// Nothing confines a call to the parameter names its tool declares: an open-parameter
    /// tool takes any object, so the *key* is attacker-chosen text. A key carried as spelled
    /// would leave classified, where no drift report would ever mention it.
    #[test]
    fn a_hostile_argument_key_does_not_escape_in_either_mode() {
        let hostile = serde_json::json!({
            "arguments": r#"{"/home/alice/.ssh/id_rsa":true,"alice@corp.example":null}"#
        });
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let mut tokens = Tokens::default();
            let stripped = strip(&hostile, &ROOT, &mut tokens, mode, &vouched());
            let rendered = serde_json::to_string(&stripped.value).expect("serializes");
            for spelled in ["id_rsa", "alice", "corp.example", "/home"] {
                assert!(!rendered.contains(spelled), "{spelled} escaped in {mode:?}: {rendered}");
            }
        }
    }

    /// Baseline offers the person one thing: the names their own policy spells. A session id,
    /// a colleague's email and a content digest are none of them, and no mode carries those.
    #[test]
    fn baseline_carries_the_deployment_s_names_and_nobody_s_identity() {
        let value = serde_json::json!({
            "tool": "Bash",
            "nested": { "tool": "Read" },
            "reader": "alice@corp.example",
            "session": "cc:6906d44d-d32f-44cc-b110-89db24c6d5db",
            "digest": "38142c4d026dba0e8f82124bf7d95f1edd7f8ab8e348f41fd276ec1af59c1a63"
        });
        let mut tokens = Tokens::default();
        let stripped = strip(&value, &ROOT, &mut tokens, Mode::Baseline, &vouched());
        assert_eq!(stripped.value["tool"], "Bash", "the policy's own vocabulary is spelled");
        assert_eq!(stripped.value["reader"], "reader-1");
        assert_eq!(stripped.value["session"], "trajectory-1");
        assert_eq!(stripped.value["digest"], "digest-1");
        let rendered = serde_json::to_string(&stripped.value).expect("serializes");
        for spelled in ["alice", "6906d44d", "38142c4d"] {
            assert!(!rendered.contains(spelled), "{spelled} survived baseline");
        }
    }

    /// A tool name is the one name in the inventory that need not have come from a policy.
    /// The harness announces whatever the model asked for, and APPA records the hook either
    /// way — so Baseline spells the name only when the deployment wrote it.
    #[test]
    fn an_undeclared_tool_name_is_the_model_s_string_and_not_the_deployment_s() {
        let value = serde_json::json!({
            "tool": "Bash",
            "nested": { "tool": "/home/alice/.ssh/id_rsa" }
        });
        let mut tokens = Tokens::default();
        let stripped = strip(&value, &ROOT, &mut tokens, Mode::Baseline, &vouched());
        assert_eq!(stripped.value["tool"], "Bash", "the policy writes this one");
        let invented = &stripped.value["nested"]["tool"];
        assert_ne!(*invented, "/home/alice/.ssh/id_rsa");
        let rendered = serde_json::to_string(&stripped.value).expect("serializes");
        for spelled in ["id_rsa", "alice", "/home"] {
            assert!(!rendered.contains(spelled), "{spelled} survived baseline: {rendered}");
        }
    }

    /// A wildcard policy writes no tool name at all, so it vouches for none: the report is
    /// still readable through tokens, and carries none of the model's spellings.
    #[test]
    fn a_deployment_that_writes_no_tool_name_spells_none_of_them() {
        let value = serde_json::json!({ "tool": "Bash", "nested": { "tool": "Bash" } });
        let mut tokens = Tokens::default();
        let stripped = strip(&value, &ROOT, &mut tokens, Mode::Baseline, &BTreeSet::new());
        assert_ne!(stripped.value["tool"], "Bash");
        assert_eq!(
            stripped.value["tool"], stripped.value["nested"]["tool"],
            "one name still reads as one tool"
        );
    }

    /// The instantiated selector in a source's answer is not the template the policy wrote:
    /// `includes($argument)` fills the placeholder from a tool call's argument.
    #[test]
    fn an_instantiated_selector_is_caller_data_in_both_modes() {
        let value = serde_json::json!({ "group": "@directory:group/finance@corp.example" });
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let mut tokens = Tokens::default();
            let stripped = strip(&value, &ROOT, &mut tokens, mode, &vouched());
            let rendered = serde_json::to_string(&stripped.value).expect("serializes");
            assert!(
                !rendered.contains("finance") && !rendered.contains("corp.example"),
                "the selector's spelling survived {mode:?}: {rendered}"
            );
        }
    }

    /// The drift report exists to name a field whose table entry is missing, and every such
    /// key is a Rust field name. A key that is not one did not come from a struct, so its
    /// spelling is data and is dropped from the document and the drift path alike.
    #[test]
    fn a_hostile_key_loses_its_spelling_even_in_the_drift_report() {
        let stripped = run(serde_json::json!({ "alice@corp.example": "whatever" }));
        assert_eq!(drift(&stripped), vec![("{key}", "Root")]);
        let rendered = serde_json::to_string(&stripped.value).expect("serializes");
        assert!(!rendered.contains("alice"), "the key survived: {rendered}");
    }

    /// A digest correlates within one report exactly as its hex did, which is the only
    /// correlation a reader has — and the only one the hex was carried for.
    #[test]
    fn a_digest_token_still_correlates_two_facts() {
        let stripped = run(serde_json::json!({
            "digest": "38142c4d026dba0e8f82124bf7d95f1edd7f8ab8e348f41fd276ec1af59c1a63",
            "nested": { "digest": "38142c4d026dba0e8f82124bf7d95f1edd7f8ab8e348f41fd276ec1af59c1a63" }
        }));
        assert_eq!(stripped.value["digest"], stripped.value["nested"]["digest"]);
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

    /// The dialect's `const`, `enum` and bounds take integers, and an integer is the widest
    /// channel in the document: a 64-bit `minimum` carries more than most strings would. The
    /// agent authors all of it, so a number is tokenized exactly as a string literal is, and
    /// no digit of it survives in either mode.
    #[test]
    fn an_authored_number_is_a_literal_and_not_arithmetic() {
        let smuggled = 4_503_599_627_370_495i64;
        let shape = serde_json::json!({
            "shape": {
                "type": "object",
                "properties": {
                    "attempt": { "type": "integer", "enum": [1, 2, 3] },
                    "version": { "type": "integer", "const": 2 },
                    "size": { "type": "integer", "minimum": smuggled, "maximum": 2 }
                },
                "required": ["attempt", "size", "version"]
            }
        });
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let mut tokens = Tokens::default();
            let stripped = strip(&shape, &ROOT, &mut tokens, mode, &vouched());
            let rendered = serde_json::to_string(&stripped.value).expect("serializes");
            assert!(
                !rendered.contains("4503599627370495"),
                "an authored bound crossed in {mode:?}: {rendered}"
            );
            assert!(stripped.unclassified.is_empty(), "a literal is classified, not drift");

            // The shape is still legible: three properties, one of them bounded at both ends,
            // and `2` and `maximum: 4` are the same literal, so they share a token.
            let properties = stripped.value["shape"]["properties"].clone();
            let fields = properties.as_object().expect("an object");
            assert_eq!(fields.len(), 3);
            let bounded = fields
                .values()
                .find(|node| node.get("minimum").is_some())
                .expect("one property carries bounds");
            assert_ne!(bounded["minimum"], bounded["maximum"]);
            let konst = fields
                .values()
                .find_map(|node| node.get("const"))
                .expect("one property is a const");
            assert_eq!(*konst, bounded["maximum"], "the same number is the same literal");
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
