//! `APPA Tool Parameters v1`: the closed schema dialect compiled into every
//! [`ToolContract`](crate::contract::ToolContract), and the engine-only canonical argument
//! path.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const MAX_SCHEMA_SOURCE_BYTES: usize = 64 * 1024;
const MAX_SCHEMA_DEPTH: usize = 16;
const MAX_SCHEMA_NODES: usize = 256;
const MAX_OBJECT_PROPERTIES: usize = 64;
const MAX_ENUM_VALUES: usize = 64;
const MAX_PROPERTY_NAME_BYTES: usize = 128;
const MAX_DESCRIPTION_SCALARS: usize = 512;

pub const MAX_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_ARGUMENT_DEPTH: usize = 32;
const MAX_ARGUMENT_NODES: usize = 4096;
const MAX_ARRAY_ELEMENTS: usize = 4096;
const MAX_NUMBER_TOKEN_BYTES: usize = 64;
const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// A dialect violation in an authored `APPA Tool Parameters v1` schema. Every variant is a
/// load error: the schema never reaches a contract.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ParamsError {
    #[error("the schema root must be an object schema")]
    RootNotObject,
    #[error("schema node must be a JSON object")]
    NodeNotObject,
    #[error("schema node must carry exactly one string `type`")]
    MissingType,
    #[error("unknown schema type {0:?}")]
    UnknownType(String),
    #[error("unknown or unsupported keyword {0:?}")]
    UnknownKeyword(String),
    #[error("keyword {keyword:?} is not valid on a {node_type} node")]
    WrongNodeKeyword { keyword: String, node_type: &'static str },
    #[error("a scalar node may use `const` or `enum`, not both")]
    ConstAndEnum,
    #[error("`enum` must be a nonempty, duplicate-free, type-correct scalar list")]
    BadEnum,
    #[error("`const` must be a type-correct scalar")]
    BadConst,
    #[error("`required` name {0:?} is duplicated or does not name a declared property")]
    BadRequired(String),
    #[error("`properties` must be an object of schema nodes")]
    BadProperties,
    #[error("`additionalProperties` must be a boolean")]
    BadAdditionalProperties,
    #[error("an array node must use one schema-valued `items`")]
    BadItems,
    #[error("`description` must be a string of at most {MAX_DESCRIPTION_SCALARS} Unicode scalar values")]
    BadDescription,
    #[error("length and item bounds must be nonnegative integers with minimum no greater than maximum")]
    BadLengthBound,
    #[error("a numeric schema may declare at most one lower and one upper bound, and the interval must be nonempty")]
    BadNumericBound,
    #[error("authored schema source exceeds {MAX_SCHEMA_SOURCE_BYTES} canonical bytes")]
    SourceTooLarge,
    #[error("schema exceeds depth {MAX_SCHEMA_DEPTH}")]
    TooDeep,
    #[error("schema exceeds {MAX_SCHEMA_NODES} nodes")]
    TooManyNodes,
    #[error("object declares more than {MAX_OBJECT_PROPERTIES} properties or required names")]
    TooManyProperties,
    #[error("property name {0:?} exceeds {MAX_PROPERTY_NAME_BYTES} UTF-8 bytes")]
    PropertyNameTooLong(String),
    #[error("persisted schema is not in normalized form")]
    NotNormalized,
}

/// A violation on the argument path: strict parsing, schema validation without
/// coercion, or a persisted payload failing canonical revalidation. Surfaces as the
/// explicit `InvalidCall` engine error — never a check block or remedy gap.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ArgumentError {
    #[error("tool-call input must be one JSON object")]
    NotAnObject,
    #[error("argument input exceeds {MAX_ARGUMENT_BYTES} bytes")]
    TooLarge,
    #[error("argument input is not valid UTF-8")]
    InvalidUtf8,
    #[error("argument input is not valid JSON: {0}")]
    Syntax(String),
    #[error("duplicate object key {0:?}")]
    DuplicateKey(String),
    #[error("trailing data after the argument object")]
    TrailingData,
    #[error("argument input exceeds depth {MAX_ARGUMENT_DEPTH}")]
    TooDeep,
    #[error("argument input exceeds {MAX_ARGUMENT_NODES} JSON nodes")]
    TooManyNodes,
    #[error("array exceeds {MAX_ARRAY_ELEMENTS} elements")]
    TooManyArrayElements,
    #[error("number token exceeds {MAX_NUMBER_TOKEN_BYTES} source bytes")]
    NumberTokenTooLong,
    #[error("integer is outside the exact safe range")]
    UnsafeInteger,
    #[error("arguments do not satisfy the tool's registered schema: {0}")]
    Schema(String),
    #[error("arguments match no registered contract")]
    NoMatchingContract,
    #[error("persisted argument payload is not in canonical form")]
    NotCanonical,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolParameters {
    root: ObjectSchema,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObjectSchema {
    description: Option<String>,
    properties: BTreeMap<String, SchemaNode>,
    required: Vec<String>,
    additional: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SchemaNode {
    Object(ObjectSchema),
    Array {
        description: Option<String>,
        items: Box<SchemaNode>,
        min_items: Option<u64>,
        max_items: Option<u64>,
    },
    String {
        description: Option<String>,
        constraint: ScalarConstraint,
        min_length: Option<u64>,
        max_length: Option<u64>,
    },
    Integer {
        description: Option<String>,
        constraint: ScalarConstraint,
        bounds: NumericBounds,
    },
    Number {
        description: Option<String>,
        constraint: ScalarConstraint,
        bounds: NumericBounds,
    },
    Boolean {
        description: Option<String>,
        constraint: ScalarConstraint,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScalarConstraint {
    Free,
    Const(Value),
    Enum(Vec<Value>),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
struct NumericBounds {
    lower: Option<(serde_json::Number, bool)>,
    upper: Option<(serde_json::Number, bool)>,
}

impl ToolParameters {
    /// Compile one authored schema. Every dialect violation is a load error; the authored
    /// source limit counts the schema's canonical JSON bytes.
    pub fn compile(authored: &Value) -> Result<Self, ParamsError> {
        let source = canonical_bytes(authored);
        if source.len() > MAX_SCHEMA_SOURCE_BYTES {
            return Err(ParamsError::SourceTooLarge);
        }
        Self::compile_unmeasured(authored)
    }

    /// The omitted-`parameters` normalization: any JSON object is accepted under
    /// the v1 global input limits, leaving the tool's argument shape untyped.
    pub fn open() -> Self {
        ToolParameters {
            root: ObjectSchema {
                description: None,
                properties: BTreeMap::new(),
                required: Vec::new(),
                additional: true,
            },
        }
    }

    fn compile_unmeasured(authored: &Value) -> Result<Self, ParamsError> {
        let mut budget = SchemaBudget::default();
        let root = match compile_node(authored, 1, &mut budget)? {
            SchemaNode::Object(root) => root,
            _ => return Err(ParamsError::RootNotObject),
        };
        Ok(ToolParameters { root })
    }

    /// The normalized schema rendering: what the contract serializes, policy identity
    /// hashes over, and a host advertises — never a second rendering of it.
    pub fn normalized(&self) -> Value {
        render_object(&self.root)
    }

    /// Validate one parsed argument object against this schema without coercion, defaults,
    /// stripping, or normalization.
    pub(crate) fn validate(&self, arguments: &Value) -> Result<(), ArgumentError> {
        validate_object(&self.root, arguments, "$")
    }

    /// Whether `name` is a required top-level string property — the shape an audience argument
    /// binding must point at: a placeholder or dynamic binding reads that
    /// argument, so the schema has to guarantee it is present and a string before any check.
    pub(crate) fn required_string_property(&self, name: &str) -> Result<(), PropertyFault> {
        match self.root.properties.get(name) {
            None => Err(PropertyFault::Undeclared),
            Some(SchemaNode::String { .. }) if self.root.required.iter().any(|required| required == name) => Ok(()),
            Some(SchemaNode::String { .. }) => Err(PropertyFault::Optional),
            Some(_) => Err(PropertyFault::NotString),
        }
    }

    /// Whether `name` is a required top-level property of any type — the shape a resolver input
    /// mapped from `$tool_call.arguments.<name>` must point at. The resolver receives whatever
    /// JSON value the argument holds, so only presence has to be guaranteed before the call is
    /// resolved. Nesting does not count: only the root object's own properties are read.
    pub(crate) fn required_property(&self, name: &str) -> Result<(), PropertyFault> {
        match self.root.properties.contains_key(name) {
            false => Err(PropertyFault::Undeclared),
            true => match self.root.required.iter().any(|required| required == name) {
                true => Ok(()),
                false => Err(PropertyFault::Optional),
            },
        }
    }
}

/// Why a top-level property is not the required string an audience argument binding needs.
/// Nesting does not count: only the root object's own properties are read.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum PropertyFault {
    #[error("is not a top-level property of the tool's `parameters`")]
    Undeclared,
    #[error("is declared but not with `type = \"string\"`")]
    NotString,
    #[error("is declared but not listed in `required`")]
    Optional,
}

impl Serialize for ToolParameters {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.normalized().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ToolParameters {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = Value::deserialize(deserializer)?;
        let compiled = ToolParameters::compile_unmeasured(&wire).map_err(serde::de::Error::custom)?;
        let normalized = canonical_bytes(&compiled.normalized());
        let presented = canonical_bytes(&wire);
        if normalized != presented {
            return Err(serde::de::Error::custom(ParamsError::NotNormalized));
        }
        Ok(compiled)
    }
}

#[derive(Default)]
struct SchemaBudget {
    nodes: usize,
}

impl SchemaBudget {
    fn spend(&mut self) -> Result<(), ParamsError> {
        self.nodes += 1;
        if self.nodes > MAX_SCHEMA_NODES {
            return Err(ParamsError::TooManyNodes);
        }
        Ok(())
    }
}

const OBJECT_KEYWORDS: &[&str] = &["type", "description", "properties", "required", "additionalProperties"];
const ARRAY_KEYWORDS: &[&str] = &["type", "description", "items", "minItems", "maxItems"];
const STRING_KEYWORDS: &[&str] = &["type", "description", "const", "enum", "minLength", "maxLength"];
const NUMERIC_KEYWORDS: &[&str] = &[
    "type",
    "description",
    "const",
    "enum",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
];
const BOOLEAN_KEYWORDS: &[&str] = &["type", "description", "const", "enum"];

fn compile_node(node: &Value, depth: usize, budget: &mut SchemaBudget) -> Result<SchemaNode, ParamsError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(ParamsError::TooDeep);
    }
    budget.spend()?;
    let map = node.as_object().ok_or(ParamsError::NodeNotObject)?;
    let node_type = match map.get("type") {
        Some(Value::String(t)) => t.as_str(),
        _ => return Err(ParamsError::MissingType),
    };
    let (keywords, label) = match node_type {
        "object" => (OBJECT_KEYWORDS, "object"),
        "array" => (ARRAY_KEYWORDS, "array"),
        "string" => (STRING_KEYWORDS, "string"),
        "integer" | "number" => (NUMERIC_KEYWORDS, "numeric"),
        "boolean" => (BOOLEAN_KEYWORDS, "boolean"),
        other => return Err(ParamsError::UnknownType(other.to_string())),
    };
    for key in map.keys() {
        if !keywords.contains(&key.as_str()) {
            let known_elsewhere = [
                OBJECT_KEYWORDS,
                ARRAY_KEYWORDS,
                STRING_KEYWORDS,
                NUMERIC_KEYWORDS,
                BOOLEAN_KEYWORDS,
            ]
            .iter()
            .any(|set| set.contains(&key.as_str()));
            return if known_elsewhere {
                Err(ParamsError::WrongNodeKeyword {
                    keyword: key.clone(),
                    node_type: label,
                })
            } else {
                Err(ParamsError::UnknownKeyword(key.clone()))
            };
        }
    }
    let description = compile_description(map)?;
    match node_type {
        "object" => {
            let mut properties = BTreeMap::new();
            if let Some(raw) = map.get("properties") {
                let raw = raw.as_object().ok_or(ParamsError::BadProperties)?;
                if raw.len() > MAX_OBJECT_PROPERTIES {
                    return Err(ParamsError::TooManyProperties);
                }
                for (name, child) in raw {
                    if name.len() > MAX_PROPERTY_NAME_BYTES {
                        return Err(ParamsError::PropertyNameTooLong(name.clone()));
                    }
                    properties.insert(name.clone(), compile_node(child, depth + 1, budget)?);
                }
            }
            let mut required = Vec::new();
            if let Some(raw) = map.get("required") {
                let raw = raw.as_array().ok_or_else(|| ParamsError::BadRequired(String::new()))?;
                if raw.len() > MAX_OBJECT_PROPERTIES {
                    return Err(ParamsError::TooManyProperties);
                }
                for name in raw {
                    let name = name
                        .as_str()
                        .ok_or_else(|| ParamsError::BadRequired(name.to_string()))?;
                    if required.iter().any(|seen: &String| seen == name) || !properties.contains_key(name) {
                        return Err(ParamsError::BadRequired(name.to_string()));
                    }
                    required.push(name.to_string());
                }
                required.sort();
            }
            let additional = match map.get("additionalProperties") {
                None => false,
                Some(Value::Bool(flag)) => *flag,
                Some(_) => return Err(ParamsError::BadAdditionalProperties),
            };
            Ok(SchemaNode::Object(ObjectSchema {
                description,
                properties,
                required,
                additional,
            }))
        }
        "array" => {
            let items = map.get("items").ok_or(ParamsError::BadItems)?;
            let items = Box::new(compile_node(items, depth + 1, budget)?);
            let min_items = compile_length_bound(map.get("minItems"))?;
            let max_items = compile_length_bound(map.get("maxItems"))?;
            if let (Some(min), Some(max)) = (min_items, max_items)
                && min > max
            {
                return Err(ParamsError::BadLengthBound);
            }
            Ok(SchemaNode::Array {
                description,
                items,
                min_items,
                max_items,
            })
        }
        "string" => {
            let constraint = compile_constraint(map, |v| v.is_string())?;
            let min_length = compile_length_bound(map.get("minLength"))?;
            let max_length = compile_length_bound(map.get("maxLength"))?;
            if let (Some(min), Some(max)) = (min_length, max_length)
                && min > max
            {
                return Err(ParamsError::BadLengthBound);
            }
            Ok(SchemaNode::String {
                description,
                constraint,
                min_length,
                max_length,
            })
        }
        "integer" => {
            let constraint = compile_constraint(map, is_schema_integer)?;
            let bounds = compile_numeric_bounds(map)?;
            Ok(SchemaNode::Integer {
                description,
                constraint,
                bounds,
            })
        }
        "number" => {
            let constraint = compile_constraint(map, |v| matches!(v, Value::Number(n) if suppliable(n)))?;
            let bounds = compile_numeric_bounds(map)?;
            Ok(SchemaNode::Number {
                description,
                constraint,
                bounds,
            })
        }
        "boolean" => {
            let constraint = compile_constraint(map, |v| v.is_boolean())?;
            Ok(SchemaNode::Boolean {
                description,
                constraint,
            })
        }
        _ => unreachable!("node_type was matched above"),
    }
}

fn compile_description(map: &serde_json::Map<String, Value>) -> Result<Option<String>, ParamsError> {
    match map.get("description") {
        None => Ok(None),
        Some(Value::String(text)) => {
            if text.chars().count() > MAX_DESCRIPTION_SCALARS {
                return Err(ParamsError::BadDescription);
            }
            Ok(Some(text.clone()))
        }
        Some(_) => Err(ParamsError::BadDescription),
    }
}

fn compile_constraint(
    map: &serde_json::Map<String, Value>,
    type_correct: impl Fn(&Value) -> bool,
) -> Result<ScalarConstraint, ParamsError> {
    match (map.get("const"), map.get("enum")) {
        (Some(_), Some(_)) => Err(ParamsError::ConstAndEnum),
        (Some(value), None) => {
            if !type_correct(value) {
                return Err(ParamsError::BadConst);
            }
            Ok(ScalarConstraint::Const(value.clone()))
        }
        (None, Some(values)) => {
            let values = values.as_array().ok_or(ParamsError::BadEnum)?;
            if values.is_empty() || values.len() > MAX_ENUM_VALUES {
                return Err(ParamsError::BadEnum);
            }
            let mut members = Vec::with_capacity(values.len());
            let mut seen = BTreeSet::new();
            for value in values {
                if !type_correct(value) {
                    return Err(ParamsError::BadEnum);
                }
                let key = canonical_bytes(value);
                if !seen.insert(key) {
                    return Err(ParamsError::BadEnum);
                }
                members.push(value.clone());
            }
            members.sort_by_cached_key(canonical_bytes);
            Ok(ScalarConstraint::Enum(members))
        }
        (None, None) => Ok(ScalarConstraint::Free),
    }
}

fn compile_length_bound(bound: Option<&Value>) -> Result<Option<u64>, ParamsError> {
    match bound {
        None => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or(ParamsError::BadLengthBound),
    }
}

fn compile_numeric_bounds(map: &serde_json::Map<String, Value>) -> Result<NumericBounds, ParamsError> {
    let bound = |key: &str| -> Result<Option<serde_json::Number>, ParamsError> {
        match map.get(key) {
            None => Ok(None),
            Some(Value::Number(n)) if suppliable(n) => Ok(Some(n.clone())),
            Some(_) => Err(ParamsError::BadNumericBound),
        }
    };
    let lower = match (bound("minimum")?, bound("exclusiveMinimum")?) {
        (Some(_), Some(_)) => return Err(ParamsError::BadNumericBound),
        (Some(n), None) => Some((n, false)),
        (None, Some(n)) => Some((n, true)),
        (None, None) => None,
    };
    let upper = match (bound("maximum")?, bound("exclusiveMaximum")?) {
        (Some(_), Some(_)) => return Err(ParamsError::BadNumericBound),
        (Some(n), None) => Some((n, false)),
        (None, Some(n)) => Some((n, true)),
        (None, None) => None,
    };
    if let (Some((low, low_exclusive)), Some((high, high_exclusive))) = (&lower, &upper) {
        let low_value = low.as_f64().ok_or(ParamsError::BadNumericBound)?;
        let high_value = high.as_f64().ok_or(ParamsError::BadNumericBound)?;
        let nonempty = low_value < high_value || (low_value == high_value && !low_exclusive && !high_exclusive);
        if !nonempty {
            return Err(ParamsError::BadNumericBound);
        }
    }
    Ok(NumericBounds { lower, upper })
}

/// Could a legal argument carry this authored number? Argument scanning refuses an
/// integer-formed token outside the safe-integer range, so a `const`, an `enum` member or a
/// bound beyond it names a value no call can supply. Inside the range every value is exact
/// in `f64`, which is what validation and scalar equality compare through; outside it, two
/// distinct authored integers can compare equal. A number authored in a non-integer form
/// carries no such bound, and the scanner applies none to it either.
fn suppliable(number: &serde_json::Number) -> bool {
    match (number.as_u64(), number.as_i64()) {
        (Some(n), _) => n <= MAX_SAFE_INTEGER as u64,
        (None, Some(n)) => n.unsigned_abs() <= MAX_SAFE_INTEGER as u64,
        (None, None) => true,
    }
}

fn is_schema_integer(value: &Value) -> bool {
    match value.as_i64() {
        Some(n) => n.unsigned_abs() <= MAX_SAFE_INTEGER as u64,
        None => false,
    }
}

fn render_object(object: &ObjectSchema) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("type".into(), "object".into());
    render_description(&mut out, &object.description);
    let mut properties = serde_json::Map::new();
    for (name, child) in &object.properties {
        properties.insert(name.clone(), render_node(child));
    }
    out.insert("properties".into(), Value::Object(properties));
    out.insert(
        "required".into(),
        object.required.iter().cloned().map(Value::from).collect(),
    );
    out.insert("additionalProperties".into(), Value::Bool(object.additional));
    Value::Object(out)
}

fn render_node(node: &SchemaNode) -> Value {
    let mut out = serde_json::Map::new();
    match node {
        SchemaNode::Object(object) => return render_object(object),
        SchemaNode::Array {
            description,
            items,
            min_items,
            max_items,
        } => {
            out.insert("type".into(), "array".into());
            render_description(&mut out, description);
            out.insert("items".into(), render_node(items));
            if let Some(min) = min_items {
                out.insert("minItems".into(), (*min).into());
            }
            if let Some(max) = max_items {
                out.insert("maxItems".into(), (*max).into());
            }
        }
        SchemaNode::String {
            description,
            constraint,
            min_length,
            max_length,
        } => {
            out.insert("type".into(), "string".into());
            render_description(&mut out, description);
            render_constraint(&mut out, constraint);
            if let Some(min) = min_length {
                out.insert("minLength".into(), (*min).into());
            }
            if let Some(max) = max_length {
                out.insert("maxLength".into(), (*max).into());
            }
        }
        SchemaNode::Integer {
            description,
            constraint,
            bounds,
        }
        | SchemaNode::Number {
            description,
            constraint,
            bounds,
        } => {
            let type_name = if matches!(node, SchemaNode::Integer { .. }) {
                "integer"
            } else {
                "number"
            };
            out.insert("type".into(), type_name.into());
            render_description(&mut out, description);
            render_constraint(&mut out, constraint);
            if let Some((n, exclusive)) = &bounds.lower {
                let key = if *exclusive { "exclusiveMinimum" } else { "minimum" };
                out.insert(key.into(), Value::Number(n.clone()));
            }
            if let Some((n, exclusive)) = &bounds.upper {
                let key = if *exclusive { "exclusiveMaximum" } else { "maximum" };
                out.insert(key.into(), Value::Number(n.clone()));
            }
        }
        SchemaNode::Boolean {
            description,
            constraint,
        } => {
            out.insert("type".into(), "boolean".into());
            render_description(&mut out, description);
            render_constraint(&mut out, constraint);
        }
    }
    Value::Object(out)
}

fn render_description(out: &mut serde_json::Map<String, Value>, description: &Option<String>) {
    if let Some(text) = description {
        out.insert("description".into(), Value::String(text.clone()));
    }
}

fn render_constraint(out: &mut serde_json::Map<String, Value>, constraint: &ScalarConstraint) {
    match constraint {
        ScalarConstraint::Free => {}
        ScalarConstraint::Const(value) => {
            out.insert("const".into(), value.clone());
        }
        ScalarConstraint::Enum(values) => {
            out.insert("enum".into(), Value::Array(values.clone()));
        }
    }
}

fn validate_object(object: &ObjectSchema, value: &Value, path: &str) -> Result<(), ArgumentError> {
    let map = value
        .as_object()
        .ok_or_else(|| ArgumentError::Schema(format!("{path}: expected an object")))?;
    for name in &object.required {
        if !map.contains_key(name) {
            return Err(ArgumentError::Schema(format!(
                "{path}: missing required property {name:?}"
            )));
        }
    }
    for (name, member) in map {
        match object.properties.get(name) {
            Some(child) => validate_node(child, member, &format!("{path}.{name}"))?,
            None if object.additional => {}
            None => {
                return Err(ArgumentError::Schema(format!("{path}: undeclared property {name:?}")));
            }
        }
    }
    Ok(())
}

fn validate_node(node: &SchemaNode, value: &Value, path: &str) -> Result<(), ArgumentError> {
    let mismatch = |expected: &str| ArgumentError::Schema(format!("{path}: expected {expected}"));
    match node {
        SchemaNode::Object(object) => validate_object(object, value, path),
        SchemaNode::Array {
            items,
            min_items,
            max_items,
            ..
        } => {
            let elements = value.as_array().ok_or_else(|| mismatch("an array"))?;
            let count = elements.len() as u64;
            if min_items.is_some_and(|min| count < min) || max_items.is_some_and(|max| count > max) {
                return Err(ArgumentError::Schema(format!(
                    "{path}: array length {count} out of bounds"
                )));
            }
            for (index, element) in elements.iter().enumerate() {
                validate_node(items, element, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        SchemaNode::String {
            constraint,
            min_length,
            max_length,
            ..
        } => {
            let text = value.as_str().ok_or_else(|| mismatch("a string"))?;
            let scalars = text.chars().count() as u64;
            if min_length.is_some_and(|min| scalars < min) || max_length.is_some_and(|max| scalars > max) {
                return Err(ArgumentError::Schema(format!(
                    "{path}: string length {scalars} out of bounds"
                )));
            }
            validate_constraint(constraint, value, path)
        }
        SchemaNode::Integer { constraint, bounds, .. } => {
            if !is_schema_integer(value) {
                return Err(mismatch("an exact safe integer"));
            }
            validate_bounds(bounds, value, path)?;
            validate_constraint(constraint, value, path)
        }
        SchemaNode::Number { constraint, bounds, .. } => {
            if !value.is_number() {
                return Err(mismatch("a number"));
            }
            validate_bounds(bounds, value, path)?;
            validate_constraint(constraint, value, path)
        }
        SchemaNode::Boolean { constraint, .. } => {
            if !value.is_boolean() {
                return Err(mismatch("a boolean"));
            }
            validate_constraint(constraint, value, path)
        }
    }
}

fn validate_bounds(bounds: &NumericBounds, value: &Value, path: &str) -> Result<(), ArgumentError> {
    let out_of_bounds = || ArgumentError::Schema(format!("{path}: number out of bounds"));
    let n = value
        .as_f64()
        .expect("a validated numeric argument is a finite binary64 value");
    if let Some((low, exclusive)) = &bounds.lower {
        let low = low
            .as_f64()
            .expect("a compiled numeric bound is a finite binary64 value");
        if n < low || (*exclusive && n == low) {
            return Err(out_of_bounds());
        }
    }
    if let Some((high, exclusive)) = &bounds.upper {
        let high = high
            .as_f64()
            .expect("a compiled numeric bound is a finite binary64 value");
        if n > high || (*exclusive && n == high) {
            return Err(out_of_bounds());
        }
    }
    Ok(())
}

fn validate_constraint(constraint: &ScalarConstraint, value: &Value, path: &str) -> Result<(), ArgumentError> {
    let ok = match constraint {
        ScalarConstraint::Free => true,
        ScalarConstraint::Const(expected) => scalar_eq(value, expected),
        ScalarConstraint::Enum(members) => members.iter().any(|member| scalar_eq(value, member)),
    };
    if ok {
        Ok(())
    } else {
        Err(ArgumentError::Schema(format!(
            "{path}: value not permitted by const/enum"
        )))
    }
}

/// Scalar equality for const/enum. Numbers compare by value, not representation: every
/// accepted number is exactly a binary64 (the scanner's safe-integer rule), and canonical
/// persistence re-parses an authored `1.0` as `1`, so representation-sensitive equality
/// would make an initially valid dispatch fail replay. Non-numbers compare exactly — a
/// string never matches a number, so no coercion enters.
fn scalar_eq(value: &Value, expected: &Value) -> bool {
    match (value.as_f64(), expected.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => value == expected,
    }
}

/// One engine-validated argument object: strictly parsed, schema-checked, carrying its
/// RFC 8785 canonical text — one digest domain, so bytes and digest cannot disagree with
/// the value. The canonical text is computed exactly once, by the validating
/// constructors, which stay the only writers: a stored rendering can never drift from
/// the value it renders, and the hot path (digest, dispatch body, authority crossing)
/// borrows it instead of re-canonicalizing per access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalArguments {
    value: Value,
    canonical: String,
}

impl CanonicalArguments {
    /// Strictly parse and canonicalize one argument object without applying a tool schema.
    /// Contract selection reads this value before the selected contract validates it.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, ArgumentError> {
        Self::from_raw_unchecked(bytes)
    }

    /// One raw JSON object, strictly scanned and schema-validated against one contract. The
    /// engine selects the contract first and validates through [`ToolParameters::validate`]; this
    /// is the test fixture path.
    #[cfg(test)]
    pub(crate) fn from_raw(bytes: &[u8], parameters: &ToolParameters) -> Result<Self, ArgumentError> {
        let unchecked = Self::from_raw_unchecked(bytes)?;
        parameters.validate(&unchecked.value)?;
        Ok(unchecked)
    }

    /// The construction path for callers that hold a parsed value. It funnels through the
    /// same scanner and validation as [`Self::from_raw`], so the two paths cannot diverge.
    #[cfg(test)]
    pub(crate) fn from_value(value: &Value, parameters: &ToolParameters) -> Result<Self, ArgumentError> {
        let bytes = serde_json::to_vec(value).map_err(|e| ArgumentError::Syntax(e.to_string()))?;
        Self::from_raw(&bytes, parameters)
    }

    fn from_raw_unchecked(bytes: &[u8]) -> Result<Self, ArgumentError> {
        let value = strict_parse(bytes)?;
        let rendered = canonical_bytes(&value);
        if rendered.len() > MAX_ARGUMENT_BYTES {
            return Err(ArgumentError::TooLarge);
        }
        let canonical = String::from_utf8(rendered).expect("canonical JSON is UTF-8");
        Ok(CanonicalArguments { value, canonical })
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        self.canonical.as_bytes()
    }

    pub fn canonical_text(&self) -> &str {
        &self.canonical
    }
}

/// A default-feature `serde_json::Value` always canonicalizes: keys are strings and every
/// representable number is a finite binary64 or exact integer.
pub fn canonical_bytes(value: &Value) -> Vec<u8> {
    serde_json_canonicalizer::to_vec(value).expect("a serde_json::Value canonicalizes")
}

/// Strictly scan and parse one JSON object without schema validation — the same token-level gate
/// the argument path runs, shared with the return-shape walk at the child boundary.
pub(crate) fn strict_parse(bytes: &[u8]) -> Result<Value, ArgumentError> {
    scan(bytes)?;
    serde_json::from_slice(bytes).map_err(|e| ArgumentError::Syntax(e.to_string()))
}

impl Serialize for CanonicalArguments {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.canonical)
    }
}

impl<'de> Deserialize<'de> for CanonicalArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct BoundedText;
        impl serde::de::Visitor<'_> for BoundedText {
            type Value = String;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(f, "a canonical argument string of at most {MAX_ARGUMENT_BYTES} bytes")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<String, E> {
                if v.len() > MAX_ARGUMENT_BYTES {
                    return Err(E::custom(ArgumentError::TooLarge));
                }
                Ok(v.to_owned())
            }

            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<String, E> {
                if v.len() > MAX_ARGUMENT_BYTES {
                    return Err(E::custom(ArgumentError::TooLarge));
                }
                Ok(v)
            }
        }
        let text = deserializer.deserialize_str(BoundedText)?;
        let unchecked = CanonicalArguments::from_raw_unchecked(text.as_bytes()).map_err(serde::de::Error::custom)?;
        if unchecked.canonical_bytes() != text.as_bytes() {
            return Err(serde::de::Error::custom(ArgumentError::NotCanonical));
        }
        Ok(unchecked)
    }
}

#[cfg(test)]
pub(crate) fn test_arguments(value: &Value) -> CanonicalArguments {
    CanonicalArguments::from_value(value, &ToolParameters::open()).expect("test arguments are dialect-valid")
}

/// Test fixture: the smallest schema an audience argument binding on `name` may point at — one
/// required top-level string property.
#[cfg(test)]
pub(crate) fn test_string_argument_schema(name: &str) -> ToolParameters {
    ToolParameters::compile(&serde_json::json!({
        "type": "object",
        "properties": { name: { "type": "string" } },
        "required": [name],
        "additionalProperties": true,
    }))
    .expect("the one-string-argument schema is dialect-valid")
}

fn scan(bytes: &[u8]) -> Result<(), ArgumentError> {
    if bytes.len() > MAX_ARGUMENT_BYTES {
        return Err(ArgumentError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ArgumentError::InvalidUtf8)?;
    let mut scanner = Scanner {
        text,
        index: 0,
        nodes: 0,
    };
    scanner.skip_whitespace();
    if !matches!(scanner.peek(), Some('{')) {
        return Err(ArgumentError::NotAnObject);
    }
    scanner.value(0)?;
    scanner.skip_whitespace();
    if scanner.index < scanner.text.len() {
        return Err(ArgumentError::TrailingData);
    }
    Ok(())
}

struct Scanner<'a> {
    text: &'a str,
    index: usize,
    nodes: usize,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.index..].chars().next()
    }

    fn bump(&mut self) -> Result<char, ArgumentError> {
        let c = self.peek().ok_or_else(|| self.syntax("unexpected end of input"))?;
        self.index += c.len_utf8();
        Ok(c)
    }

    fn syntax(&self, message: &str) -> ArgumentError {
        ArgumentError::Syntax(format!("{message} at byte {}", self.index))
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if matches!(c, ' ' | '\t' | '\n' | '\r') {
                self.index += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), ArgumentError> {
        let got = self.bump()?;
        if got != expected {
            return Err(self.syntax(&format!("expected {expected:?}")));
        }
        Ok(())
    }

    /// `depth` and the node budget both count containers (objects and arrays): scalars
    /// are already bounded by the byte, string, and number-token limits, and counting
    /// them as nodes would make the array-element limit unreachable — a dead check.
    fn value(&mut self, depth: usize) -> Result<(), ArgumentError> {
        self.skip_whitespace();
        match self.peek().ok_or_else(|| self.syntax("unexpected end of input"))? {
            '{' | '[' if depth >= MAX_ARGUMENT_DEPTH => Err(ArgumentError::TooDeep),
            c @ ('{' | '[') => {
                self.nodes += 1;
                if self.nodes > MAX_ARGUMENT_NODES {
                    return Err(ArgumentError::TooManyNodes);
                }
                if c == '{' {
                    self.object(depth)
                } else {
                    self.array(depth)
                }
            }
            '"' => self.string().map(|_| ()),
            't' => self.literal("true"),
            'f' => self.literal("false"),
            'n' => self.literal("null"),
            '-' | '0'..='9' => self.number(),
            _ => Err(self.syntax("unexpected character")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<(), ArgumentError> {
        self.expect('{')?;
        self.skip_whitespace();
        let mut keys = BTreeSet::new();
        if matches!(self.peek(), Some('}')) {
            self.index += 1;
            return Ok(());
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            if !keys.insert(key.clone()) {
                return Err(ArgumentError::DuplicateKey(key));
            }
            self.skip_whitespace();
            self.expect(':')?;
            self.value(depth + 1)?;
            self.skip_whitespace();
            match self.bump()? {
                ',' => continue,
                '}' => return Ok(()),
                _ => return Err(self.syntax("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<(), ArgumentError> {
        self.expect('[')?;
        self.skip_whitespace();
        if matches!(self.peek(), Some(']')) {
            self.index += 1;
            return Ok(());
        }
        let mut elements = 0usize;
        loop {
            elements += 1;
            if elements > MAX_ARRAY_ELEMENTS {
                return Err(ArgumentError::TooManyArrayElements);
            }
            self.value(depth + 1)?;
            self.skip_whitespace();
            match self.bump()? {
                ',' => continue,
                ']' => return Ok(()),
                _ => return Err(self.syntax("expected ',' or ']'")),
            }
        }
    }

    fn string(&mut self) -> Result<String, ArgumentError> {
        self.expect('"')?;
        let mut decoded = String::new();
        loop {
            let c = self.bump()?;
            match c {
                '"' => break,
                '\\' => {
                    let escape = self.bump()?;
                    match escape {
                        '"' => decoded.push('"'),
                        '\\' => decoded.push('\\'),
                        '/' => decoded.push('/'),
                        'b' => decoded.push('\u{0008}'),
                        'f' => decoded.push('\u{000C}'),
                        'n' => decoded.push('\n'),
                        'r' => decoded.push('\r'),
                        't' => decoded.push('\t'),
                        'u' => decoded.push(self.unicode_escape()?),
                        _ => return Err(self.syntax("invalid escape")),
                    }
                }
                c if (c as u32) < 0x20 => return Err(self.syntax("unescaped control character")),
                c => decoded.push(c),
            }
        }
        Ok(decoded)
    }

    fn unicode_escape(&mut self) -> Result<char, ArgumentError> {
        let unit = self.hex4()?;
        if (0xD800..=0xDBFF).contains(&unit) {
            // A high surrogate must pair with an escaped low surrogate.
            self.expect('\\').map_err(|_| self.syntax("unpaired surrogate"))?;
            self.expect('u').map_err(|_| self.syntax("unpaired surrogate"))?;
            let low = self.hex4()?;
            if !(0xDC00..=0xDFFF).contains(&low) {
                return Err(self.syntax("unpaired surrogate"));
            }
            let scalar = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
            char::from_u32(scalar).ok_or_else(|| self.syntax("invalid unicode escape"))
        } else if (0xDC00..=0xDFFF).contains(&unit) {
            Err(self.syntax("unpaired surrogate"))
        } else {
            char::from_u32(unit).ok_or_else(|| self.syntax("invalid unicode escape"))
        }
    }

    fn hex4(&mut self) -> Result<u32, ArgumentError> {
        let mut unit = 0u32;
        for _ in 0..4 {
            let digit = self
                .bump()?
                .to_digit(16)
                .ok_or_else(|| self.syntax("invalid unicode escape"))?;
            unit = unit * 16 + digit;
        }
        Ok(unit)
    }

    fn literal(&mut self, expected: &str) -> Result<(), ArgumentError> {
        for want in expected.chars() {
            if self.bump()? != want {
                return Err(self.syntax("invalid literal"));
            }
        }
        Ok(())
    }

    fn number(&mut self) -> Result<(), ArgumentError> {
        let start = self.index;
        if matches!(self.peek(), Some('-')) {
            self.index += 1;
        }
        match self.peek() {
            Some('0') => self.index += 1,
            Some('1'..='9') => self.digits(),
            _ => return Err(self.syntax("invalid number")),
        }
        let integer_end = self.index;
        let mut integer_form = true;
        if matches!(self.peek(), Some('.')) {
            integer_form = false;
            self.index += 1;
            if !matches!(self.peek(), Some('0'..='9')) {
                return Err(self.syntax("invalid number"));
            }
            self.digits();
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            integer_form = false;
            self.index += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.index += 1;
            }
            if !matches!(self.peek(), Some('0'..='9')) {
                return Err(self.syntax("invalid number"));
            }
            self.digits();
        }
        if self.index - start > MAX_NUMBER_TOKEN_BYTES {
            return Err(ArgumentError::NumberTokenTooLong);
        }
        if integer_form {
            // An integer-formed token must be an exact safe integer; a wider literal is an
            // unsupported numeric form even when it fits the token-length limit.
            let token = &self.text[start..integer_end];
            let parsed: i128 = token.parse().map_err(|_| ArgumentError::UnsafeInteger)?;
            // unsigned_abs: `abs` would overflow (and panic) on i128::MIN, which fits the
            // 64-byte token limit and is exactly an input this check must reject.
            if parsed.unsigned_abs() > MAX_SAFE_INTEGER as u128 {
                return Err(ArgumentError::UnsafeInteger);
            }
        }
        Ok(())
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some('0'..='9')) {
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn compile(schema: serde_json::Value) -> Result<ToolParameters, ParamsError> {
        ToolParameters::compile(&schema)
    }

    fn args(value: serde_json::Value) -> Result<CanonicalArguments, ArgumentError> {
        CanonicalArguments::from_value(&value, &ToolParameters::open())
    }

    fn raw(bytes: &[u8]) -> Result<CanonicalArguments, ArgumentError> {
        CanonicalArguments::from_raw(bytes, &ToolParameters::open())
    }

    // --- the compiler: closed dialect -----------------------------------------------

    #[test]
    fn a_conforming_schema_compiles_and_an_authored_gap_normalizes() {
        let compiled = compile(json!({
            "type": "object",
            "properties": { "to": { "type": "string" } },
            "required": ["to"],
        }))
        .unwrap();
        // Omitted additionalProperties normalizes to the closed default.
        assert_eq!(
            compiled.normalized(),
            json!({
                "type": "object",
                "properties": { "to": { "type": "string" } },
                "required": ["to"],
                "additionalProperties": false,
            })
        );
    }

    #[test]
    fn every_foreign_schema_form_is_a_load_error() {
        // Boolean schema, type array, null type, unknown keyword, wrong-node keyword,
        // $ref, pattern, format, tuple items, schema-valued additionalProperties, default.
        assert_eq!(compile(json!(true)), Err(ParamsError::NodeNotObject));
        assert_eq!(
            compile(json!({ "type": ["object", "string"] })),
            Err(ParamsError::MissingType)
        );
        assert_eq!(
            compile(json!({ "type": "null" })),
            Err(ParamsError::UnknownType("null".into()))
        );
        assert_eq!(
            compile(json!({ "type": "object", "title": "x" })),
            Err(ParamsError::UnknownKeyword("title".into()))
        );
        assert_eq!(
            compile(json!({ "type": "object", "minLength": 1 })),
            Err(ParamsError::WrongNodeKeyword {
                keyword: "minLength".into(),
                node_type: "object"
            })
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "$ref": "#/x" } } })),
            Err(ParamsError::MissingType)
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "string", "$ref": "#/x" } } })),
            Err(ParamsError::UnknownKeyword("$ref".into()))
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "string", "pattern": ".*" } } })),
            Err(ParamsError::UnknownKeyword("pattern".into()))
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "string", "format": "email" } } })),
            Err(ParamsError::UnknownKeyword("format".into()))
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": {
                "x": { "type": "array", "items": [{ "type": "string" }] } } })),
            Err(ParamsError::NodeNotObject)
        );
        assert_eq!(
            compile(json!({ "type": "object", "additionalProperties": { "type": "string" } })),
            Err(ParamsError::BadAdditionalProperties)
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "string", "default": "d" } } })),
            Err(ParamsError::UnknownKeyword("default".into()))
        );
    }

    #[test]
    fn a_scalar_node_carries_const_xor_enum() {
        assert_eq!(
            compile(json!({ "type": "object", "properties": {
                "x": { "type": "string", "const": "a", "enum": ["a"] } } })),
            Err(ParamsError::ConstAndEnum)
        );
        // Type-correct const; a wrong-typed const refuses.
        assert!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "string", "const": "a" } } })).is_ok()
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "string", "const": 1 } } })),
            Err(ParamsError::BadConst)
        );
        // Enum: nonempty, duplicate-free, type-correct; an integer const must be a safe integer.
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "integer", "enum": [] } } })),
            Err(ParamsError::BadEnum)
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "integer", "enum": [1, 1] } } })),
            Err(ParamsError::BadEnum)
        );
        assert_eq!(
            compile(
                json!({ "type": "object", "properties": { "x": { "type": "integer", "const": 9007199254740992u64 } } })
            ),
            Err(ParamsError::BadConst)
        );
        // i64::MIN is out of the safe range and must refuse without overflowing.
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "integer", "const": i64::MIN } } })),
            Err(ParamsError::BadConst)
        );
    }

    /// A `number` schema had no domain of its own, so it could name a constant no legal
    /// argument can carry — argument scanning refuses an integer-formed token past the safe
    /// range — and, compared through `f64`, that constant equals its supported neighbour.
    #[test]
    fn a_number_schema_names_only_values_an_argument_could_carry() {
        let scalar = |keyword: &str, value: serde_json::Value| {
            compile(json!({ "type": "object", "properties": {
                "x": { "type": "number", keyword: value } } }))
        };
        assert_eq!(scalar("const", json!(9007199254740993u64)), Err(ParamsError::BadConst));
        assert_eq!(scalar("const", json!(i64::MIN)), Err(ParamsError::BadConst));
        assert_eq!(
            scalar("enum", json!([1, 9007199254740993u64])),
            Err(ParamsError::BadEnum)
        );
        assert_eq!(
            scalar("minimum", json!(9007199254740993u64)),
            Err(ParamsError::BadNumericBound)
        );
        assert_eq!(
            scalar("exclusiveMaximum", json!(i64::MIN)),
            Err(ParamsError::BadNumericBound)
        );

        // The edge of the range, and a non-integer form, both stay authorable: the scanner
        // bounds an integer-formed token and nothing else.
        assert!(scalar("const", json!(9007199254740991u64)).is_ok());
        assert!(scalar("const", json!(-9007199254740991i64)).is_ok());
        assert!(scalar("const", json!(1.5e300)).is_ok());
        assert!(scalar("maximum", json!(1.5)).is_ok());
    }

    #[test]
    fn numeric_constraints_survive_canonical_round_trips() {
        // `const: 1.0` and an argument `1` are the same binary64 value; canonical
        // persistence re-parses `1.0` as `1`, so representation-sensitive equality would
        // make an initially valid payload fail replay revalidation.
        let schema = compile(json!({
            "type": "object",
            "properties": { "x": { "type": "number", "const": 1.0 } },
        }))
        .unwrap();
        let fresh = CanonicalArguments::from_value(&json!({ "x": 1.0 }), &schema).unwrap();
        let wire = serde_json::to_string(&fresh).unwrap();
        let replayed: CanonicalArguments = serde_json::from_str(&wire).unwrap();
        assert!(schema.validate(replayed.value()).is_ok());
        assert!(CanonicalArguments::from_value(&json!({ "x": 1 }), &schema).is_ok());
        assert!(CanonicalArguments::from_value(&json!({ "x": 2 }), &schema).is_err());
    }

    #[test]
    fn required_names_must_be_unique_and_declared() {
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "a": { "type": "string" } }, "required": ["a", "a"] })),
            Err(ParamsError::BadRequired("a".into()))
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": {}, "required": ["ghost"] })),
            Err(ParamsError::BadRequired("ghost".into()))
        );
        // Authored order is free; the normalized rendering sorts by Unicode scalar value.
        let compiled = compile(json!({
            "type": "object",
            "properties": { "a": { "type": "string" }, "b": { "type": "string" } },
            "required": ["b", "a"],
        }))
        .unwrap();
        assert_eq!(compiled.normalized()["required"], json!(["a", "b"]));
    }

    #[test]
    fn bounds_must_form_nonempty_intervals() {
        assert_eq!(
            compile(json!({ "type": "object", "properties": {
                "x": { "type": "string", "minLength": 3, "maxLength": 2 } } })),
            Err(ParamsError::BadLengthBound)
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": {
                "x": { "type": "string", "minLength": -1 } } })),
            Err(ParamsError::BadLengthBound)
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": {
                "x": { "type": "number", "minimum": 1, "exclusiveMinimum": 2 } } })),
            Err(ParamsError::BadNumericBound)
        );
        assert_eq!(
            compile(json!({ "type": "object", "properties": {
                "x": { "type": "number", "exclusiveMinimum": 2, "maximum": 2 } } })),
            Err(ParamsError::BadNumericBound)
        );
        // Equal inclusive bounds are a one-point interval — legal.
        assert!(
            compile(json!({ "type": "object", "properties": {
            "x": { "type": "number", "minimum": 2, "maximum": 2 } } }))
            .is_ok()
        );
        // An array node must carry one schema-valued items.
        assert_eq!(
            compile(json!({ "type": "object", "properties": { "x": { "type": "array" } } })),
            Err(ParamsError::BadItems)
        );
    }

    #[test]
    fn schema_limits_hold_at_threshold_and_refuse_one_over() {
        // Depth: a chain of nested objects with a string leaf, root = depth 1.
        let nest = |levels: usize| {
            let mut node = json!({ "type": "string" });
            for _ in 1..levels {
                node = json!({ "type": "object", "properties": { "c": node } });
            }
            node
        };
        assert!(compile(nest(MAX_SCHEMA_DEPTH)).is_ok());
        assert_eq!(compile(nest(MAX_SCHEMA_DEPTH + 1)), Err(ParamsError::TooDeep));

        // Properties per object.
        let props = |count: usize| {
            let members: serde_json::Map<String, Value> = (0..count)
                .map(|i| (format!("p{i:03}"), json!({ "type": "boolean" })))
                .collect();
            json!({ "type": "object", "properties": members })
        };
        assert!(compile(props(MAX_OBJECT_PROPERTIES)).is_ok());
        assert_eq!(
            compile(props(MAX_OBJECT_PROPERTIES + 1)),
            Err(ParamsError::TooManyProperties)
        );

        // Property name bytes.
        let named = |len: usize| json!({ "type": "object", "properties": { "a".repeat(len): { "type": "boolean" } } });
        assert!(compile(named(MAX_PROPERTY_NAME_BYTES)).is_ok());
        assert_eq!(
            compile(named(MAX_PROPERTY_NAME_BYTES + 1)),
            Err(ParamsError::PropertyNameTooLong(
                "a".repeat(MAX_PROPERTY_NAME_BYTES + 1)
            ))
        );

        // Description scalars (counted in Unicode scalar values, not bytes).
        let described = |len: usize| json!({ "type": "object", "description": "é".repeat(len) });
        assert!(compile(described(MAX_DESCRIPTION_SCALARS)).is_ok());
        assert_eq!(
            compile(described(MAX_DESCRIPTION_SCALARS + 1)),
            Err(ParamsError::BadDescription)
        );

        // Enum size.
        let wide_enum = |count: usize| {
            let members: Vec<Value> = (0..count).map(|i| json!(i as i64)).collect();
            json!({ "type": "object", "properties": { "x": { "type": "integer", "enum": members } } })
        };
        assert!(compile(wide_enum(MAX_ENUM_VALUES)).is_ok());
        assert_eq!(compile(wide_enum(MAX_ENUM_VALUES + 1)), Err(ParamsError::BadEnum));

        // Node count: a root with 4 objects of 62 leaves each (1 + 4×63 = 253 nodes) plus
        // exact leaf padding — 256 compiles, 257 refuses.
        let budget = |root_leaves: usize| {
            let mut members: serde_json::Map<String, Value> = (0..4)
                .map(|i| {
                    let leaves: serde_json::Map<String, Value> = (0..62)
                        .map(|k| (format!("g{i}k{k}"), json!({ "type": "boolean" })))
                        .collect();
                    (format!("g{i}"), json!({ "type": "object", "properties": leaves }))
                })
                .collect();
            for pad in 0..root_leaves {
                members.insert(format!("pad{pad}"), json!({ "type": "boolean" }));
            }
            json!({ "type": "object", "properties": members })
        };
        assert!(compile(budget(MAX_SCHEMA_NODES - 253)).is_ok());
        assert_eq!(compile(budget(MAX_SCHEMA_NODES - 252)), Err(ParamsError::TooManyNodes));
    }

    #[test]
    fn the_authored_source_limit_counts_canonical_bytes_and_deserialize_does_not_reapply_it() {
        // Build a legal schema whose authored canonical form sits just under 64 KiB while
        // its normalized rendering (defaults inserted per object node) crosses it: the
        // authored measure applies at compile only (`Q30`), so the normalized form still
        // round-trips through serde.
        let build = |description_len: usize| {
            let members: serde_json::Map<String, Value> = (0..60)
                .map(|i| {
                    (
                        format!("p{i:02}"),
                        json!({ "type": "object", "properties": {
                            "s": { "type": "string", "description": "d".repeat(description_len) },
                            "t": { "type": "string", "description": "d".repeat(description_len) } } }),
                    )
                })
                .collect();
            json!({ "type": "object", "properties": members })
        };
        let mut description_len = 400;
        let authored = loop {
            let candidate = build(description_len);
            let size = canonical_bytes(&candidate).len();
            if size > MAX_SCHEMA_SOURCE_BYTES {
                break build(description_len - 1);
            }
            description_len += 1;
        };
        let compiled = ToolParameters::compile(&authored).expect("authored form is under the limit");
        let normalized_size = canonical_bytes(&compiled.normalized()).len();
        assert!(
            normalized_size > MAX_SCHEMA_SOURCE_BYTES,
            "normalization must cross the authored measure for this test to bite"
        );
        let wire = serde_json::to_string(&compiled).unwrap();
        let restored: ToolParameters = serde_json::from_str(&wire).unwrap();
        assert_eq!(restored, compiled);

        // One byte over at compile time still refuses.
        let over = build(512);
        assert!(canonical_bytes(&over).len() > MAX_SCHEMA_SOURCE_BYTES);
        assert_eq!(ToolParameters::compile(&over), Err(ParamsError::SourceTooLarge));
    }

    #[test]
    fn normalization_is_deterministic_across_authored_spellings() {
        let a = compile(json!({
            "type": "object",
            "required": ["b", "a"],
            "properties": { "b": { "type": "string" }, "a": { "type": "integer", "enum": [3, 1, 2] } },
        }))
        .unwrap();
        let b = compile(json!({
            "properties": { "a": { "enum": [2, 3, 1], "type": "integer" }, "b": { "type": "string" } },
            "required": ["a", "b"],
            "type": "object",
        }))
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(canonical_bytes(&a.normalized()), canonical_bytes(&b.normalized()));
        assert_eq!(a.normalized()["properties"]["a"]["enum"], json!([1, 2, 3]));
    }

    #[test]
    fn deserialize_accepts_only_the_exact_normalized_form() {
        // The authored spelling (defaults omitted) is not the normalized rendering: the
        // persisted-schema path refuses it, so a schema cannot skip compilation.
        let authored = json!({ "type": "object", "properties": { "a": { "type": "string" } } });
        let compiled = ToolParameters::compile(&authored).unwrap();
        let round: ToolParameters = serde_json::from_value(compiled.normalized()).unwrap();
        assert_eq!(round, compiled);
        assert!(serde_json::from_value::<ToolParameters>(authored).is_err());
        assert!(serde_json::from_value::<ToolParameters>(json!({ "type": "string" })).is_err());
    }

    // --- the audience-binding read ------------------------------------------

    #[test]
    fn a_binding_target_is_a_required_top_level_string_and_nothing_else() {
        let schema = compile(json!({
            "type": "object",
            "properties": {
                "to": { "type": "string" },
                "channel": { "type": "string", "enum": ["ops", "dev"] },
                "kind": { "type": "string", "const": "email" },
                "cc": { "type": "string" },
                "count": { "type": "integer" },
                "meta": {
                    "type": "object",
                    "properties": { "owner": { "type": "string" } },
                    "required": ["owner"]
                }
            },
            "required": ["to", "channel", "kind", "count", "meta"],
            "additionalProperties": true,
        }))
        .unwrap();
        assert_eq!(schema.required_string_property("to"), Ok(()));
        assert_eq!(schema.required_string_property("channel"), Ok(()));
        assert_eq!(schema.required_string_property("kind"), Ok(()));
        assert_eq!(schema.required_string_property("cc"), Err(PropertyFault::Optional));
        assert_eq!(schema.required_string_property("count"), Err(PropertyFault::NotString));
        assert_eq!(schema.required_string_property("meta"), Err(PropertyFault::NotString));
        // Nesting does not count: `owner` is required inside `meta`, not at the top level.
        assert_eq!(schema.required_string_property("owner"), Err(PropertyFault::Undeclared));
        assert_eq!(schema.required_string_property("bcc"), Err(PropertyFault::Undeclared));
        // The omitted-`parameters` default declares nothing, so it can host no binding.
        assert_eq!(
            ToolParameters::open().required_string_property("to"),
            Err(PropertyFault::Undeclared)
        );
    }

    // --- the strict argument path ----------------------------------------------------------

    #[test]
    fn the_scanner_rejects_what_serde_json_absorbs() {
        assert_eq!(raw(br#"{"a":1,"a":2}"#), Err(ArgumentError::DuplicateKey("a".into())));
        assert_eq!(raw(br#"{"a":1} trailing"#), Err(ArgumentError::TrailingData));
        assert_eq!(raw(br#"{"a":1}{}"#), Err(ArgumentError::TrailingData));
        assert!(matches!(raw(br#"{"a":"\q"}"#), Err(ArgumentError::Syntax(_))));
        assert!(matches!(raw(br#"{"a":"\ud800"}"#), Err(ArgumentError::Syntax(_))));
        assert!(matches!(raw(b"[1]"), Err(ArgumentError::NotAnObject)));
        assert!(matches!(raw(b"\"s\""), Err(ArgumentError::NotAnObject)));
        assert_eq!(raw(&[0xff, 0xfe]), Err(ArgumentError::InvalidUtf8));
    }

    #[test]
    fn number_forms_are_bounded_and_exactly_representable() {
        // 2^53-1 is the largest exact safe integer; one over is an unsupported numeric form.
        assert!(raw(br#"{"n":9007199254740991}"#).is_ok());
        assert_eq!(raw(br#"{"n":9007199254740992}"#), Err(ArgumentError::UnsafeInteger));
        assert_eq!(raw(br#"{"n":-9007199254740992}"#), Err(ArgumentError::UnsafeInteger));
        // i128::MIN fits the 64-byte token limit and must refuse without overflowing.
        assert_eq!(
            raw(br#"{"n":-170141183460469231731687303715884105728}"#),
            Err(ArgumentError::UnsafeInteger)
        );
        // Fraction and exponent forms are binary64, not integers — in range they pass.
        assert!(raw(br#"{"n":1e30}"#).is_ok());
        assert!(matches!(raw(br#"{"n":1e999}"#), Err(ArgumentError::Syntax(_))));
        // A number token wider than 64 source bytes refuses regardless of value.
        let long_token = format!("{{\"n\":0.{}}}", "3".repeat(70));
        assert_eq!(raw(long_token.as_bytes()), Err(ArgumentError::NumberTokenTooLong));
    }

    #[test]
    fn input_limits_hold_at_threshold_and_refuse_one_over() {
        // Depth (the root object is depth 1).
        let nested = |depth: usize| {
            let mut text = String::new();
            for _ in 0..depth {
                text.push_str("{\"a\":");
            }
            text.push('1');
            text.push_str(&"}".repeat(depth));
            text
        };
        assert!(raw(nested(MAX_ARGUMENT_DEPTH).as_bytes()).is_ok());
        assert_eq!(
            raw(nested(MAX_ARGUMENT_DEPTH + 1).as_bytes()),
            Err(ArgumentError::TooDeep)
        );

        // Array elements.
        let wide = |count: usize| format!("{{\"a\":[{}]}}", vec!["1"; count].join(","));
        assert!(raw(wide(MAX_ARRAY_ELEMENTS).as_bytes()).is_ok());
        assert_eq!(
            raw(wide(MAX_ARRAY_ELEMENTS + 1).as_bytes()),
            Err(ArgumentError::TooManyArrayElements)
        );

        let noded = |count: usize| format!("{{\"a\":[{}]}}", vec!["[]"; count - 2].join(","));
        assert!(raw(noded(MAX_ARGUMENT_NODES).as_bytes()).is_ok());
        assert_eq!(
            raw(noded(MAX_ARGUMENT_NODES + 1).as_bytes()),
            Err(ArgumentError::TooManyNodes)
        );

        // Total input bytes.
        let padded = format!("{{\"a\":\"{}\"}}", "x".repeat(MAX_ARGUMENT_BYTES + 1));
        assert_eq!(raw(padded.as_bytes()), Err(ArgumentError::TooLarge));
    }

    #[test]
    fn schema_validation_neither_coerces_nor_defaults_nor_strips() {
        let closed = compile(json!({
            "type": "object",
            "properties": { "to": { "type": "string" }, "n": { "type": "integer" } },
            "required": ["to"],
        }))
        .unwrap();
        // No coercion: a number is not a string, a float form is not an integer.
        assert!(CanonicalArguments::from_value(&json!({ "to": "a" }), &closed).is_ok());
        assert!(matches!(
            CanonicalArguments::from_value(&json!({ "to": 1 }), &closed),
            Err(ArgumentError::Schema(_))
        ));
        assert!(matches!(
            CanonicalArguments::from_value(&json!({ "to": "a", "n": 1.5 }), &closed),
            Err(ArgumentError::Schema(_))
        ));
        // No defaults: a missing required property refuses rather than being inserted.
        assert!(matches!(
            CanonicalArguments::from_value(&json!({}), &closed),
            Err(ArgumentError::Schema(_))
        ));
        // No stripping: an undeclared property refuses under the closed default.
        assert!(matches!(
            CanonicalArguments::from_value(&json!({ "to": "a", "extra": true }), &closed),
            Err(ArgumentError::Schema(_))
        ));
        // The open schema admits arbitrary bounded members, null included.
        assert!(args(json!({ "anything": [null, { "deep": true }] })).is_ok());
    }

    #[test]
    fn const_and_enum_validate_by_exact_value() {
        let constrained = compile(json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "enum": ["fast", "slow"] },
                "version": { "type": "integer", "const": 1 },
            },
        }))
        .unwrap();
        assert!(CanonicalArguments::from_value(&json!({ "mode": "fast", "version": 1 }), &constrained).is_ok());
        assert!(matches!(
            CanonicalArguments::from_value(&json!({ "mode": "medium" }), &constrained),
            Err(ArgumentError::Schema(_))
        ));
        assert!(matches!(
            CanonicalArguments::from_value(&json!({ "version": 1.0 }), &constrained),
            Err(ArgumentError::Schema(_))
        ));
    }

    // --- RFC 8785 canonical bytes and the digest domain -------------------------------------

    #[test]
    #[allow(clippy::excessive_precision)]
    fn canonical_bytes_follow_rfc_8785() {
        let value = args(json!({
            "numbers": [333333333.33333329f64, 1e30, 4.5, 0.002, 1e-27],
            "literals": [null, true, false],
            "n": 1.0,
        }))
        .unwrap();
        assert_eq!(
            value.canonical_text().to_owned(),
            r#"{"literals":[null,true,false],"n":1,"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27]}"#
        );
    }

    #[test]
    fn canonical_expansion_cannot_create_an_unreplayable_payload() {
        let raw = format!(
            "{{{}}}",
            (0..9_000)
                .map(|index| format!(r#""k{index}":1e20"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(raw.len() < MAX_ARGUMENT_BYTES);
        assert_eq!(
            CanonicalArguments::from_raw(raw.as_bytes(), &ToolParameters::open()),
            Err(ArgumentError::TooLarge)
        );
    }

    #[test]
    fn key_order_is_utf16_code_units_not_code_points() {
        let value = args(json!({ "\u{fb33}": 1, "\u{1f600}": 2 })).unwrap();
        assert_eq!(value.canonical_text().to_owned(), "{\"\u{1f600}\":2,\"\u{fb33}\":1}");
    }

    #[test]
    fn the_compat_and_raw_paths_share_one_digest_domain() {
        let from_raw = raw(b"{ \"b\" : 2, \"a\" : 1 }").unwrap();
        let from_value = args(json!({ "a": 1, "b": 2 })).unwrap();
        assert_eq!(from_raw, from_value);
        assert_eq!(from_raw.canonical_bytes(), br#"{"a":1,"b":2}"#);
        assert_ne!(
            raw(br#"{"a":1}"#).unwrap().canonical_bytes(),
            raw(br#"{"a":2}"#).unwrap().canonical_bytes()
        );
    }

    // --- canonical persistence -------------------------------------------------------------

    #[test]
    fn persisted_arguments_round_trip_and_refuse_tampering() {
        let value = args(json!({ "to": "hr", "n": [1, 2] })).unwrap();
        let wire = serde_json::to_string(&value).unwrap();
        let restored: CanonicalArguments = serde_json::from_str(&wire).unwrap();
        assert_eq!(restored, value);
        // The wire form is the canonical text itself.
        assert_eq!(wire, "\"{\\\"n\\\":[1,2],\\\"to\\\":\\\"hr\\\"}\"");
        assert!(serde_json::from_str::<CanonicalArguments>("\"{\\\"to\\\":\\\"hr\\\",\\\"n\\\":[1,2]}\"").is_err());
        assert!(serde_json::from_str::<CanonicalArguments>("\"{\\\"a\\\":1,\\\"a\\\":1}\"").is_err());
        assert!(serde_json::from_str::<CanonicalArguments>("\"[1]\"").is_err());
        assert!(serde_json::from_str::<CanonicalArguments>("\"{\\\"a\\\": 1}\"").is_err());
    }
}
