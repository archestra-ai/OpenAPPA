//! `ReturnShape`: the canonical bounded structured-return type, compiled from the
//! strict JSON Schema subset a parent agent authors as `return_schema` in its `fork` call and
//! persisted whole on `ForkPrepared`.
//!
//! The dialect is deliberately narrower than `APPA Tool Parameters v1` (`params.rs`), and the two
//! must not converge: tool parameters bound *inputs* and may stay open (free strings, open
//! objects), while a return shape bounds a *quarantine exit* — every leaf is shape-bounded, so the
//! adversary who wrote what the child read can only select among declared values, never carry
//! free text. Supported leaves are booleans, exact bounded integers, bounded decimals
//! of declared precision, closed literal enums, and formats from the engine-declared closed
//! list; bounded arrays and closed objects compose them. Free strings, unbounded or
//! precision-free numbers, open objects, optional fields, unbounded collections, references,
//! combinators, unknown keywords, and unsupported formats are refused.
//!
//! The caps below are immutable engine settings, not `[limits]` configuration: they
//! bound nesting, fields, enum members, declared array length, literal size, numeric precision,
//! and the schema's canonical bytes. A `return_schema` that does not compile refuses the marked
//! spawn call as an invalid call; shape mismatch of a submitted return is a typed
//! refusal at the boundary, decided by the caller.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::params::ArgumentError;

const MAX_SHAPE_SOURCE_BYTES: usize = 8 * 1024;
const MAX_SHAPE_DEPTH: usize = 8;
const MAX_SHAPE_NODES: usize = 128;
const MAX_SHAPE_OBJECT_FIELDS: usize = 32;
const MAX_SHAPE_ENUM_MEMBERS: usize = 32;
const MAX_SHAPE_LITERAL_BYTES: usize = 256;
const MAX_SHAPE_ARRAY_ITEMS: u64 = 256;
const MAX_SHAPE_DECIMAL_PLACES: u8 = 6;

/// A violation in an authored `return_schema`. Every variant refuses the marked spawn call as an
/// invalid call; none becomes a check block or remedy gap.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ShapeError {
    #[error("the return schema root must be a closed object schema")]
    RootNotObject,
    #[error("schema node must be a JSON object")]
    NodeNotObject,
    #[error("schema node must carry exactly one string `type`")]
    MissingType,
    #[error("type {0:?} is outside the return-shape dialect")]
    UnsupportedType(String),
    #[error("keyword {0:?} is outside the return-shape dialect on this node")]
    UnsupportedKeyword(String),
    #[error(
        "a string leaf must carry exactly one of `format`, `enum`, or `const` — free strings are never shape-bounded"
    )]
    FreeString,
    #[error("format {0:?} is not in the engine-declared closed list")]
    UnsupportedFormat(String),
    #[error("`format` must be the string name of a format from the engine-declared closed list")]
    BadFormatType,
    #[error("`enum` must be a nonempty, duplicate-free, type-correct literal list")]
    BadEnum,
    #[error("`const` must be a type-correct literal")]
    BadConst,
    #[error("an integer leaf must declare `minimum` and `maximum` as exact safe integers with minimum <= maximum")]
    BadIntegerBounds,
    #[error(
        "a number leaf must declare its precision as `multipleOf` 10^-k for k in 1..={MAX_SHAPE_DECIMAL_PLACES} — a number is shape-bounded only within a declared range and precision"
    )]
    BadNumberPrecision,
    #[error(
        "a number leaf must declare `minimum` and `maximum` as exact decimals of the declared precision, within the safe range, with minimum <= maximum"
    )]
    BadNumberBounds,
    #[error("an array node must declare schema-valued `items` and a `maxItems` between 1 and {MAX_SHAPE_ARRAY_ITEMS}")]
    BadArrayBound,
    #[error("`properties` must be a nonempty object of schema nodes")]
    BadProperties,
    #[error("`required` must list every declared property exactly once — optional fields are outside the dialect")]
    BadRequired,
    #[error("`description` must be a string of at most {MAX_SHAPE_LITERAL_BYTES} UTF-8 bytes")]
    BadDescription,
    #[error("literal exceeds {MAX_SHAPE_LITERAL_BYTES} UTF-8 bytes")]
    LiteralTooLong,
    #[error("authored return schema exceeds {MAX_SHAPE_SOURCE_BYTES} canonical bytes")]
    SourceTooLarge,
    #[error("return schema exceeds depth {MAX_SHAPE_DEPTH}")]
    TooDeep,
    #[error("return schema exceeds {MAX_SHAPE_NODES} nodes")]
    TooManyNodes,
    #[error("object declares more than {MAX_SHAPE_OBJECT_FIELDS} fields")]
    TooManyFields,
    #[error("`enum` declares more than {MAX_SHAPE_ENUM_MEMBERS} members")]
    TooManyEnumMembers,
    #[error("persisted return shape is not in normalized form")]
    NotNormalized,
}

/// Why a submitted return does not fit the fork's stored shape. Strict parsing reuses the
/// argument scanner's rules; the schema walk rejects without coercion. The caller decides the
/// consequence: a typed refusal that appends nothing, with the shape fed back to the child
/// whose return the parent declared through `attest-schema`.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReturnMismatch {
    #[error("the submitted return is not one strict JSON object: {0}")]
    Parse(ArgumentError),
    #[error("the submitted return does not satisfy the fork's return shape: {0}")]
    Schema(String),
}

/// One compiled, normalized return shape — frozen content on `ForkPrepared`. Only
/// [`ReturnShape::compile`] constructs it, and `Deserialize` refuses anything but the exact
/// normalized rendering, so a shape that skipped compilation is unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnShape {
    root: ObjectShape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObjectShape {
    description: Option<String>,
    fields: BTreeMap<String, ShapeNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ShapeNode {
    Object(ObjectShape),
    Array {
        description: Option<String>,
        items: Box<ShapeNode>,
        max_items: u64,
    },
    Boolean {
        description: Option<String>,
    },
    Integer {
        description: Option<String>,
        constraint: IntegerConstraint,
    },
    Number {
        description: Option<String>,
        places: u8,
        minimum_scaled: i64,
        maximum_scaled: i64,
    },
    String {
        description: Option<String>,
        constraint: StringConstraint,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum IntegerConstraint {
    Bounds { minimum: i64, maximum: i64 },
    Const(i64),
    Enum(Vec<i64>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StringConstraint {
    Format(ShapeFormat),
    Const(String),
    Enum(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShapeFormat {
    Date,
    DateTime,
    Uuid,
}

impl ShapeFormat {
    fn from_name(name: &str) -> Option<ShapeFormat> {
        match name {
            "date" => Some(ShapeFormat::Date),
            "date-time" => Some(ShapeFormat::DateTime),
            "uuid" => Some(ShapeFormat::Uuid),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            ShapeFormat::Date => "date",
            ShapeFormat::DateTime => "date-time",
            ShapeFormat::Uuid => "uuid",
        }
    }

    fn matches(self, text: &str) -> bool {
        match self {
            ShapeFormat::Date => valid_date(text),
            ShapeFormat::DateTime => valid_date_time(text),
            ShapeFormat::Uuid => valid_uuid(text),
        }
    }
}

impl ReturnShape {
    /// Compile one authored `return_schema`. Every dialect violation refuses the marked spawn
    /// call; the source limit counts the schema's canonical JSON bytes.
    pub fn compile(authored: &Value) -> Result<Self, ShapeError> {
        if crate::params::canonical_bytes(authored).len() > MAX_SHAPE_SOURCE_BYTES {
            return Err(ShapeError::SourceTooLarge);
        }
        Self::compile_unmeasured(authored)
    }

    fn compile_unmeasured(authored: &Value) -> Result<Self, ShapeError> {
        let mut budget = ShapeBudget::default();
        match compile_node(authored, 1, &mut budget)? {
            ShapeNode::Object(root) => Ok(ReturnShape { root }),
            _ => Err(ShapeError::RootNotObject),
        }
    }

    /// The normalized schema rendering — what `ForkPrepared` persists and replay re-derives
    /// (the fork record stores the full normalized shape, not a caller claim).
    pub fn normalized(&self) -> Value {
        render_object(&self.root)
    }

    /// Validate one submitted non-void return: strict parse, schema walk without
    /// coercion, then the RFC 8785 canonical text of the accepted object.
    pub fn validate(&self, body: &str) -> Result<String, ReturnMismatch> {
        let value = crate::params::strict_parse(body.as_bytes()).map_err(ReturnMismatch::Parse)?;
        validate_object(&self.root, &value, &NodePath::Root).map_err(ReturnMismatch::Schema)?;
        let canonical = crate::params::canonical_bytes(&value);
        Ok(String::from_utf8(canonical).expect("canonical JSON is UTF-8"))
    }
}

impl Serialize for ReturnShape {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.normalized().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ReturnShape {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = Value::deserialize(deserializer)?;
        let compiled = ReturnShape::compile(&wire).map_err(serde::de::Error::custom)?;
        let normalized = crate::params::canonical_bytes(&compiled.normalized());
        let presented = crate::params::canonical_bytes(&wire);
        if normalized != presented {
            return Err(serde::de::Error::custom(ShapeError::NotNormalized));
        }
        Ok(compiled)
    }
}

#[derive(Default)]
struct ShapeBudget {
    nodes: usize,
}

impl ShapeBudget {
    fn spend(&mut self) -> Result<(), ShapeError> {
        self.nodes += 1;
        if self.nodes > MAX_SHAPE_NODES {
            return Err(ShapeError::TooManyNodes);
        }
        Ok(())
    }
}

const OBJECT_KEYWORDS: &[&str] = &["type", "description", "properties", "required"];
const ARRAY_KEYWORDS: &[&str] = &["type", "description", "items", "maxItems"];
const BOOLEAN_KEYWORDS: &[&str] = &["type", "description"];
const INTEGER_KEYWORDS: &[&str] = &["type", "description", "minimum", "maximum", "const", "enum"];
const NUMBER_KEYWORDS: &[&str] = &["type", "description", "minimum", "maximum", "multipleOf"];
const STRING_KEYWORDS: &[&str] = &["type", "description", "format", "const", "enum"];

fn compile_node(node: &Value, depth: usize, budget: &mut ShapeBudget) -> Result<ShapeNode, ShapeError> {
    if depth > MAX_SHAPE_DEPTH {
        return Err(ShapeError::TooDeep);
    }
    budget.spend()?;
    let map = node.as_object().ok_or(ShapeError::NodeNotObject)?;
    let node_type = match map.get("type") {
        Some(Value::String(t)) => t.as_str(),
        _ => return Err(ShapeError::MissingType),
    };
    let keywords = match node_type {
        "object" => OBJECT_KEYWORDS,
        "array" => ARRAY_KEYWORDS,
        "boolean" => BOOLEAN_KEYWORDS,
        "integer" => INTEGER_KEYWORDS,
        "number" => NUMBER_KEYWORDS,
        "string" => STRING_KEYWORDS,
        other => return Err(ShapeError::UnsupportedType(other.to_string())),
    };
    if let Some(key) = map.keys().find(|key| !keywords.contains(&key.as_str())) {
        return Err(ShapeError::UnsupportedKeyword(key.clone()));
    }
    let description = compile_description(map.get("description"))?;
    match node_type {
        "object" => {
            let fields = match map.get("properties") {
                Some(Value::Object(properties)) if !properties.is_empty() => properties,
                _ => return Err(ShapeError::BadProperties),
            };
            if fields.len() > MAX_SHAPE_OBJECT_FIELDS {
                return Err(ShapeError::TooManyFields);
            }
            for name in fields.keys() {
                if name.len() > MAX_SHAPE_LITERAL_BYTES {
                    return Err(ShapeError::LiteralTooLong);
                }
            }
            let required: Vec<&str> = match map.get("required") {
                Some(Value::Array(names)) => names
                    .iter()
                    .map(|name| name.as_str().ok_or(ShapeError::BadRequired))
                    .collect::<Result<_, _>>()?,
                _ => return Err(ShapeError::BadRequired),
            };
            let unique: BTreeSet<&str> = required.iter().copied().collect();
            if unique.len() != required.len() || unique.len() != fields.len() {
                return Err(ShapeError::BadRequired);
            }
            if !fields.keys().all(|name| unique.contains(name.as_str())) {
                return Err(ShapeError::BadRequired);
            }
            let mut compiled = BTreeMap::new();
            for (name, field) in fields {
                compiled.insert(name.clone(), compile_node(field, depth + 1, budget)?);
            }
            Ok(ShapeNode::Object(ObjectShape {
                description,
                fields: compiled,
            }))
        }
        "array" => {
            let items = map.get("items").ok_or(ShapeError::BadArrayBound)?;
            let max_items = match map.get("maxItems").and_then(Value::as_u64) {
                Some(bound) if (1..=MAX_SHAPE_ARRAY_ITEMS).contains(&bound) => bound,
                _ => return Err(ShapeError::BadArrayBound),
            };
            Ok(ShapeNode::Array {
                description,
                items: Box::new(compile_node(items, depth + 1, budget)?),
                max_items,
            })
        }
        "boolean" => Ok(ShapeNode::Boolean { description }),
        "integer" => Ok(ShapeNode::Integer {
            description,
            constraint: compile_integer_constraint(map)?,
        }),
        "number" => {
            let places = map
                .get("multipleOf")
                .and_then(Value::as_f64)
                .and_then(decimal_places)
                .ok_or(ShapeError::BadNumberPrecision)?;
            let minimum = map
                .get("minimum")
                .and_then(Value::as_f64)
                .and_then(|x| scaled_decimal(x, places))
                .ok_or(ShapeError::BadNumberBounds)?;
            let maximum = map
                .get("maximum")
                .and_then(Value::as_f64)
                .and_then(|x| scaled_decimal(x, places))
                .ok_or(ShapeError::BadNumberBounds)?;
            if minimum > maximum {
                return Err(ShapeError::BadNumberBounds);
            }
            Ok(ShapeNode::Number {
                description,
                places,
                minimum_scaled: minimum,
                maximum_scaled: maximum,
            })
        }
        "string" => Ok(ShapeNode::String {
            description,
            constraint: compile_string_constraint(map)?,
        }),
        _ => unreachable!("the type gate above is exhaustive"),
    }
}

fn compile_description(value: Option<&Value>) -> Result<Option<String>, ShapeError> {
    match value {
        None => Ok(None),
        Some(Value::String(text)) if text.len() <= MAX_SHAPE_LITERAL_BYTES => Ok(Some(text.clone())),
        Some(_) => Err(ShapeError::BadDescription),
    }
}

fn compile_integer_constraint(map: &serde_json::Map<String, Value>) -> Result<IntegerConstraint, ShapeError> {
    let bounds = (map.get("minimum"), map.get("maximum"));
    match (map.get("const"), map.get("enum"), bounds) {
        (Some(literal), None, (None, None)) => Ok(IntegerConstraint::Const(
            safe_integer(literal).ok_or(ShapeError::BadConst)?,
        )),
        (None, Some(Value::Array(members)), (None, None)) => {
            if members.is_empty() || members.len() > MAX_SHAPE_ENUM_MEMBERS {
                return Err(if members.is_empty() {
                    ShapeError::BadEnum
                } else {
                    ShapeError::TooManyEnumMembers
                });
            }
            let mut literals = Vec::with_capacity(members.len());
            for member in members {
                literals.push(safe_integer(member).ok_or(ShapeError::BadEnum)?);
            }
            literals.sort_unstable();
            let before = literals.len();
            literals.dedup();
            if literals.len() != before {
                return Err(ShapeError::BadEnum);
            }
            Ok(IntegerConstraint::Enum(literals))
        }
        (None, None, (Some(minimum), Some(maximum))) => {
            let minimum = safe_integer(minimum).ok_or(ShapeError::BadIntegerBounds)?;
            let maximum = safe_integer(maximum).ok_or(ShapeError::BadIntegerBounds)?;
            if minimum > maximum {
                return Err(ShapeError::BadIntegerBounds);
            }
            Ok(IntegerConstraint::Bounds { minimum, maximum })
        }
        _ => Err(ShapeError::BadIntegerBounds),
    }
}

fn compile_string_constraint(map: &serde_json::Map<String, Value>) -> Result<StringConstraint, ShapeError> {
    match (map.get("format"), map.get("const"), map.get("enum")) {
        (Some(Value::String(name)), None, None) => Ok(StringConstraint::Format(
            ShapeFormat::from_name(name).ok_or_else(|| ShapeError::UnsupportedFormat(name.clone()))?,
        )),
        (Some(_), None, None) => Err(ShapeError::BadFormatType),
        (None, Some(Value::String(literal)), None) => {
            if literal.len() > MAX_SHAPE_LITERAL_BYTES {
                return Err(ShapeError::LiteralTooLong);
            }
            Ok(StringConstraint::Const(literal.clone()))
        }
        (None, Some(_), None) => Err(ShapeError::BadConst),
        (None, None, Some(Value::Array(members))) => {
            if members.is_empty() || members.len() > MAX_SHAPE_ENUM_MEMBERS {
                return Err(if members.is_empty() {
                    ShapeError::BadEnum
                } else {
                    ShapeError::TooManyEnumMembers
                });
            }
            let mut literals = Vec::with_capacity(members.len());
            for member in members {
                let literal = member.as_str().ok_or(ShapeError::BadEnum)?;
                if literal.len() > MAX_SHAPE_LITERAL_BYTES {
                    return Err(ShapeError::LiteralTooLong);
                }
                literals.push(literal.to_string());
            }
            literals.sort_unstable();
            let before = literals.len();
            literals.dedup();
            if literals.len() != before {
                return Err(ShapeError::BadEnum);
            }
            Ok(StringConstraint::Enum(literals))
        }
        (None, None, Some(_)) => Err(ShapeError::BadEnum),
        _ => Err(ShapeError::FreeString),
    }
}

fn safe_integer(value: &Value) -> Option<i64> {
    const MAX_SAFE: i64 = (1 << 53) - 1;
    let n = value.as_i64()?;
    (-MAX_SAFE..=MAX_SAFE).contains(&n).then_some(n)
}

fn decimal_places(step: f64) -> Option<u8> {
    (1..=MAX_SHAPE_DECIMAL_PLACES).find(|k| {
        step == match k {
            1 => 0.1,
            2 => 0.01,
            3 => 0.001,
            4 => 0.000_1,
            5 => 0.000_01,
            6 => 0.000_001,
            _ => unreachable!("the range is 1..=MAX_SHAPE_DECIMAL_PLACES"),
        }
    })
}

fn scaled_decimal(x: f64, places: u8) -> Option<i64> {
    const MAX_SAFE_SCALED: i64 = (1 << 52) - 1;
    if !x.is_finite() {
        return None;
    }
    let rendered = format!("{x:.prec$}", prec = usize::from(places));
    if rendered.parse::<f64>().ok()? != x {
        return None;
    }
    let unsigned = rendered.strip_prefix('-').unwrap_or(&rendered);
    let digits: String = unsigned.chars().filter(|c| *c != '.').collect();
    let magnitude: i64 = digits.parse().ok()?;
    if magnitude > MAX_SAFE_SCALED {
        return None;
    }
    Some(if rendered.starts_with('-') {
        -magnitude
    } else {
        magnitude
    })
}

fn decimal_number(scaled: i64, places: u8) -> Value {
    let mut divisor = 1f64;
    for _ in 0..places {
        divisor *= 10f64;
    }
    Value::Number(serde_json::Number::from_f64(scaled as f64 / divisor).expect("a finite quotient"))
}

fn render_object(object: &ObjectShape) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("type".into(), Value::String("object".into()));
    if let Some(description) = &object.description {
        map.insert("description".into(), Value::String(description.clone()));
    }
    let mut fields = serde_json::Map::new();
    for (name, node) in &object.fields {
        fields.insert(name.clone(), render_node(node));
    }
    map.insert("properties".into(), Value::Object(fields));
    map.insert(
        "required".into(),
        Value::Array(object.fields.keys().map(|name| Value::String(name.clone())).collect()),
    );
    Value::Object(map)
}

fn render_node(node: &ShapeNode) -> Value {
    match node {
        ShapeNode::Object(object) => render_object(object),
        ShapeNode::Array {
            description,
            items,
            max_items,
        } => {
            let mut map = serde_json::Map::new();
            map.insert("type".into(), Value::String("array".into()));
            if let Some(description) = description {
                map.insert("description".into(), Value::String(description.clone()));
            }
            map.insert("items".into(), render_node(items));
            map.insert("maxItems".into(), Value::Number((*max_items).into()));
            Value::Object(map)
        }
        ShapeNode::Boolean { description } => {
            let mut map = serde_json::Map::new();
            map.insert("type".into(), Value::String("boolean".into()));
            if let Some(description) = description {
                map.insert("description".into(), Value::String(description.clone()));
            }
            Value::Object(map)
        }
        ShapeNode::Integer {
            description,
            constraint,
        } => {
            let mut map = serde_json::Map::new();
            map.insert("type".into(), Value::String("integer".into()));
            if let Some(description) = description {
                map.insert("description".into(), Value::String(description.clone()));
            }
            match constraint {
                IntegerConstraint::Bounds { minimum, maximum } => {
                    map.insert("minimum".into(), Value::Number((*minimum).into()));
                    map.insert("maximum".into(), Value::Number((*maximum).into()));
                }
                IntegerConstraint::Const(literal) => {
                    map.insert("const".into(), Value::Number((*literal).into()));
                }
                IntegerConstraint::Enum(literals) => {
                    map.insert(
                        "enum".into(),
                        Value::Array(literals.iter().map(|n| Value::Number((*n).into())).collect()),
                    );
                }
            }
            Value::Object(map)
        }
        ShapeNode::Number {
            description,
            places,
            minimum_scaled,
            maximum_scaled,
        } => {
            let mut map = serde_json::Map::new();
            map.insert("type".into(), Value::String("number".into()));
            if let Some(description) = description {
                map.insert("description".into(), Value::String(description.clone()));
            }
            map.insert("minimum".into(), decimal_number(*minimum_scaled, *places));
            map.insert("maximum".into(), decimal_number(*maximum_scaled, *places));
            map.insert("multipleOf".into(), decimal_number(1, *places));
            Value::Object(map)
        }
        ShapeNode::String {
            description,
            constraint,
        } => {
            let mut map = serde_json::Map::new();
            map.insert("type".into(), Value::String("string".into()));
            if let Some(description) = description {
                map.insert("description".into(), Value::String(description.clone()));
            }
            match constraint {
                StringConstraint::Format(format) => {
                    map.insert("format".into(), Value::String(format.name().into()));
                }
                StringConstraint::Const(literal) => {
                    map.insert("const".into(), Value::String(literal.clone()));
                }
                StringConstraint::Enum(literals) => {
                    map.insert(
                        "enum".into(),
                        Value::Array(literals.iter().map(|s| Value::String(s.clone())).collect()),
                    );
                }
            }
            Value::Object(map)
        }
    }
}

enum NodePath<'a> {
    Root,
    Field { parent: &'a NodePath<'a>, name: &'a str },
    Index { parent: &'a NodePath<'a>, index: usize },
}

impl std::fmt::Display for NodePath<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodePath::Root => write!(f, "$"),
            NodePath::Field { parent, name } => write!(f, "{parent}.{name}"),
            NodePath::Index { parent, index } => write!(f, "{parent}[{index}]"),
        }
    }
}

fn validate_object(object: &ObjectShape, value: &Value, path: &NodePath<'_>) -> Result<(), String> {
    let map = match value {
        Value::Object(map) => map,
        _ => return Err(format!("{path}: expected an object")),
    };
    for name in map.keys() {
        if !object.fields.contains_key(name) {
            return Err(format!("{path}: undeclared field {name:?}"));
        }
    }
    for (name, node) in &object.fields {
        let field = map.get(name).ok_or_else(|| format!("{path}: missing field {name:?}"))?;
        validate_node(node, field, &NodePath::Field { parent: path, name })?;
    }
    Ok(())
}

fn validate_node(node: &ShapeNode, value: &Value, path: &NodePath<'_>) -> Result<(), String> {
    match node {
        ShapeNode::Object(object) => validate_object(object, value, path),
        ShapeNode::Array { items, max_items, .. } => {
            let elements = match value {
                Value::Array(elements) => elements,
                _ => return Err(format!("{path}: expected an array")),
            };
            if elements.len() as u64 > *max_items {
                return Err(format!("{path}: more than {max_items} items"));
            }
            for (index, element) in elements.iter().enumerate() {
                validate_node(items, element, &NodePath::Index { parent: path, index })?;
            }
            Ok(())
        }
        ShapeNode::Boolean { .. } => match value {
            Value::Bool(_) => Ok(()),
            _ => Err(format!("{path}: expected a boolean")),
        },
        ShapeNode::Integer { constraint, .. } => {
            let n = safe_integer(value).ok_or_else(|| format!("{path}: expected an exact safe integer"))?;
            match constraint {
                IntegerConstraint::Bounds { minimum, maximum } if (*minimum..=*maximum).contains(&n) => Ok(()),
                IntegerConstraint::Const(literal) if n == *literal => Ok(()),
                IntegerConstraint::Enum(literals) if literals.binary_search(&n).is_ok() => Ok(()),
                _ => Err(format!("{path}: integer {n} is outside the declared bound")),
            }
        }
        ShapeNode::Number {
            places,
            minimum_scaled,
            maximum_scaled,
            ..
        } => {
            let scaled = value
                .as_f64()
                .and_then(|x| scaled_decimal(x, *places))
                .ok_or_else(|| format!("{path}: expected a number of at most {places} decimal places"))?;
            if (*minimum_scaled..=*maximum_scaled).contains(&scaled) {
                Ok(())
            } else {
                Err(format!("{path}: number is outside the declared bound"))
            }
        }
        ShapeNode::String { constraint, .. } => {
            let text = match value {
                Value::String(text) => text.as_str(),
                _ => return Err(format!("{path}: expected a string")),
            };
            match constraint {
                StringConstraint::Format(format) if format.matches(text) => Ok(()),
                StringConstraint::Format(format) => Err(format!("{path}: not a valid {} value", format.name())),
                StringConstraint::Const(literal) if text == literal => Ok(()),
                StringConstraint::Enum(literals) if literals.binary_search_by(|lit| lit.as_str().cmp(text)).is_ok() => {
                    Ok(())
                }
                _ => Err(format!("{path}: string is outside the declared literals")),
            }
        }
    }
}

fn valid_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let slice = &text[range];
        slice.bytes().all(|b| b.is_ascii_digit()).then(|| slice.parse().ok())?
    };
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return false;
    };
    (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

fn valid_date_time(text: &str) -> bool {
    let Some((date, rest)) = text.split_once('T') else {
        return false;
    };
    if !valid_date(date) {
        return false;
    }
    let bytes = rest.as_bytes();
    if bytes.len() < 8 || bytes[2] != b':' || bytes[5] != b':' {
        return false;
    }
    let two = |at: usize| -> Option<u32> {
        let slice = rest.get(at..at + 2)?;
        slice.bytes().all(|b| b.is_ascii_digit()).then(|| slice.parse().ok())?
    };
    let (Some(hour), Some(minute), Some(second)) = (two(0), two(3), two(6)) else {
        return false;
    };
    // Second 60 admits a leap second, as RFC 3339 does.
    if hour > 23 || minute > 59 || second > 60 {
        return false;
    }
    let mut index = 8;
    if bytes.get(index) == Some(&b'.') {
        let fraction = bytes[index + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if !(1..=9).contains(&fraction) {
            return false;
        }
        index += 1 + fraction;
    }
    match bytes.get(index) {
        Some(b'Z') => index + 1 == bytes.len(),
        Some(b'+' | b'-') => {
            let offset = &rest[index + 1..];
            let bytes = offset.as_bytes();
            if bytes.len() != 5 || bytes[2] != b':' {
                return false;
            }
            let two = |at: usize| -> Option<u32> {
                let slice = offset.get(at..at + 2)?;
                slice.bytes().all(|b| b.is_ascii_digit()).then(|| slice.parse().ok())?
            };
            matches!((two(0), two(3)), (Some(hh), Some(mm)) if hh <= 23 && mm <= 59)
        }
        _ => false,
    }
}

fn valid_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => *byte == b'-',
        _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shape(authored: serde_json::Value) -> ReturnShape {
        ReturnShape::compile(&authored).expect("the fixture schema compiles")
    }

    fn verdict_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "verdict": { "type": "string", "enum": ["allow", "deny", "escalate"] },
                "confidence": { "type": "integer", "minimum": 0, "maximum": 100 },
                "flagged": { "type": "boolean" },
                "seen": { "type": "string", "format": "date" },
            },
            "required": ["verdict", "confidence", "flagged", "seen"],
        })
    }

    #[test]
    fn a_bounded_structure_compiles_and_admits_its_values() {
        let shape = shape(json!({
            "type": "object",
            "properties": {
                "rows": {
                    "type": "array",
                    "items": verdict_schema(),
                    "maxItems": 4,
                },
            },
            "required": ["rows"],
        }));
        let body = json!({
            "rows": [
                { "verdict": "allow", "confidence": 97, "flagged": false, "seen": "2026-02-28" },
            ],
        });
        let canonical = shape
            .validate(&body.to_string())
            .expect("the conforming return validates");
        assert_eq!(
            canonical,
            r#"{"rows":[{"confidence":97,"flagged":false,"seen":"2026-02-28","verdict":"allow"}]}"#
        );
    }

    #[test]
    fn free_text_carriage_is_unrepresentable() {
        let free = json!({
            "type": "object",
            "properties": { "note": { "type": "string" } },
            "required": ["note"],
        });
        assert_eq!(ReturnShape::compile(&free), Err(ShapeError::FreeString));
        let email = json!({
            "type": "object",
            "properties": { "note": { "type": "string", "format": "email" } },
            "required": ["note"],
        });
        assert_eq!(
            ReturnShape::compile(&email),
            Err(ShapeError::UnsupportedFormat("email".into()))
        );
        let null = json!({
            "type": "object",
            "properties": { "score": { "type": "null" } },
            "required": ["score"],
        });
        assert_eq!(
            ReturnShape::compile(&null),
            Err(ShapeError::UnsupportedType("null".into()))
        );
    }

    #[test]
    fn open_and_unbounded_forms_are_refused() {
        let open = json!({
            "type": "object",
            "properties": { "flag": { "type": "boolean" } },
            "required": ["flag"],
            "additionalProperties": true,
        });
        assert_eq!(
            ReturnShape::compile(&open),
            Err(ShapeError::UnsupportedKeyword("additionalProperties".into()))
        );
        let optional = json!({
            "type": "object",
            "properties": { "flag": { "type": "boolean" } },
            "required": [],
        });
        assert_eq!(ReturnShape::compile(&optional), Err(ShapeError::BadRequired));
        let unbounded = json!({
            "type": "object",
            "properties": { "rows": { "type": "array", "items": { "type": "boolean" } } },
            "required": ["rows"],
        });
        assert_eq!(ReturnShape::compile(&unbounded), Err(ShapeError::BadArrayBound));
        let combinator = json!({
            "type": "object",
            "properties": { "flag": { "type": "boolean", "anyOf": [] } },
            "required": ["flag"],
        });
        assert_eq!(
            ReturnShape::compile(&combinator),
            Err(ShapeError::UnsupportedKeyword("anyOf".into()))
        );
        let reference = json!({
            "type": "object",
            "properties": { "flag": { "$ref": "#/defs/flag" } },
            "required": ["flag"],
        });
        assert_eq!(ReturnShape::compile(&reference), Err(ShapeError::MissingType));
        let unbounded_integer = json!({
            "type": "object",
            "properties": { "count": { "type": "integer", "minimum": 0 } },
            "required": ["count"],
        });
        assert_eq!(
            ReturnShape::compile(&unbounded_integer),
            Err(ShapeError::BadIntegerBounds)
        );
    }

    #[test]
    fn every_engine_cap_refuses_its_excess() {
        let mut nested = json!({ "type": "boolean" });
        for _ in 0..MAX_SHAPE_DEPTH {
            nested = json!({
                "type": "object",
                "properties": { "inner": nested },
                "required": ["inner"],
            });
        }
        assert_eq!(ReturnShape::compile(&nested), Err(ShapeError::TooDeep));

        let fields: serde_json::Map<String, Value> = (0..=MAX_SHAPE_OBJECT_FIELDS)
            .map(|i| (format!("f{i}"), json!({ "type": "boolean" })))
            .collect();
        let names: Vec<Value> = fields.keys().map(|k| Value::String(k.clone())).collect();
        let wide = json!({ "type": "object", "properties": fields, "required": names });
        assert_eq!(ReturnShape::compile(&wide), Err(ShapeError::TooManyFields));

        let members: Vec<Value> = (0..=MAX_SHAPE_ENUM_MEMBERS as i64).map(Value::from).collect();
        let fat_enum = json!({
            "type": "object",
            "properties": { "pick": { "type": "integer", "enum": members } },
            "required": ["pick"],
        });
        assert_eq!(ReturnShape::compile(&fat_enum), Err(ShapeError::TooManyEnumMembers));

        let over_bound = json!({
            "type": "object",
            "properties": {
                "rows": { "type": "array", "items": { "type": "boolean" }, "maxItems": MAX_SHAPE_ARRAY_ITEMS + 1 },
            },
            "required": ["rows"],
        });
        assert_eq!(ReturnShape::compile(&over_bound), Err(ShapeError::BadArrayBound));

        let big_literal = "x".repeat(MAX_SHAPE_SOURCE_BYTES);
        let heavy = json!({
            "type": "object",
            "properties": { "pick": { "type": "string", "const": big_literal } },
            "required": ["pick"],
        });
        assert_eq!(ReturnShape::compile(&heavy), Err(ShapeError::SourceTooLarge));
    }

    #[test]
    fn a_nonconforming_return_is_named_by_path() {
        let shape = shape(verdict_schema());
        let cases = [
            (
                json!({ "verdict": "allow", "confidence": 97, "flagged": false }),
                "missing",
            ),
            (
                json!({ "verdict": "allow", "confidence": 97, "flagged": false, "seen": "2026-02-28", "extra": 1 }),
                "undeclared",
            ),
            (
                json!({ "verdict": "maybe", "confidence": 97, "flagged": false, "seen": "2026-02-28" }),
                "literals",
            ),
            (
                json!({ "verdict": "allow", "confidence": 101, "flagged": false, "seen": "2026-02-28" }),
                "bound",
            ),
            (
                json!({ "verdict": "allow", "confidence": 97.5, "flagged": false, "seen": "2026-02-28" }),
                "integer",
            ),
            (
                json!({ "verdict": "allow", "confidence": 97, "flagged": "no", "seen": "2026-02-28" }),
                "boolean",
            ),
            (
                json!({ "verdict": "allow", "confidence": 97, "flagged": false, "seen": "2027-02-29" }),
                "date",
            ),
        ];
        for (body, expected) in cases {
            let mismatch = shape
                .validate(&body.to_string())
                .expect_err("the return must not validate");
            match mismatch {
                ReturnMismatch::Schema(message) => {
                    assert!(message.contains(expected), "{message:?} should name {expected:?}")
                }
                ReturnMismatch::Parse(error) => panic!("expected a schema mismatch, got parse error {error}"),
            }
        }
    }

    #[test]
    fn the_strict_parse_gate_precedes_the_schema_walk() {
        let shape = shape(verdict_schema());
        for body in [
            r#"{"verdict":"allow","verdict":"deny","confidence":1,"flagged":false,"seen":"2026-01-01"}"#,
            r#"{"verdict":"allow","confidence":1,"flagged":false,"seen":"2026-01-01"} tail"#,
            r#"["verdict"]"#,
        ] {
            assert!(matches!(shape.validate(body), Err(ReturnMismatch::Parse(_))));
        }
    }

    #[test]
    fn formats_admit_one_spelling_per_value() {
        assert!(valid_date("2024-02-29"));
        assert!(!valid_date("2023-02-29"));
        assert!(!valid_date("2024-13-01"));
        assert!(!valid_date("2024-1-01"));
        assert!(valid_date_time("2026-08-14T22:14:07Z"));
        assert!(valid_date_time("2026-08-14T22:14:07.250+02:00"));
        assert!(valid_date_time("2026-06-30T23:59:60Z"));
        assert!(!valid_date_time("2026-08-14t22:14:07z"));
        assert!(!valid_date_time("2026-08-14T22:14:07"));
        assert!(!valid_date_time("2026-08-14T22:14:07.Z"));
        assert!(!valid_date_time("2026-08-14T24:00:00Z"));
        assert!(valid_uuid("6f2a1c9e-3b4d-4a5e-8f60-0123456789ab"));
        assert!(!valid_uuid("6F2A1C9E-3B4D-4A5E-8F60-0123456789AB"));
        assert!(!valid_uuid("6f2a1c9e3b4d4a5e8f600123456789ab"));
    }

    #[test]
    fn only_the_normalized_rendering_deserializes() {
        let compiled = shape(verdict_schema());
        let wire = serde_json::to_string(&compiled).expect("a shape serializes");
        let reread: ReturnShape = serde_json::from_str(&wire).expect("the normalized rendering deserializes");
        assert_eq!(reread, compiled);

        let denormalized = json!({
            "type": "object",
            "properties": { "pick": { "type": "string", "enum": ["b", "a"] } },
            "required": ["pick"],
        });
        let compiled = ReturnShape::compile(&denormalized).expect("an unsorted enum compiles, sorted");
        assert_eq!(compiled.normalized()["properties"]["pick"]["enum"], json!(["a", "b"]));
        assert!(serde_json::from_value::<ReturnShape>(denormalized).is_err());

        let fields: serde_json::Map<String, Value> = (0..MAX_SHAPE_OBJECT_FIELDS)
            .map(|i| {
                (
                    format!("f{i:02}"),
                    json!({ "const": "a".repeat(MAX_SHAPE_LITERAL_BYTES), "type": "string" }),
                )
            })
            .collect();
        let names: Vec<Value> = fields.keys().map(|k| Value::String(k.clone())).collect();
        let heavy = json!({ "properties": fields, "required": names, "type": "object" });
        assert!(crate::params::canonical_bytes(&heavy).len() > MAX_SHAPE_SOURCE_BYTES);
        assert!(serde_json::from_value::<ReturnShape>(heavy).is_err());
    }

    #[test]
    fn a_bounded_decimal_compiles_and_admits_its_values() {
        let shape = shape(json!({
            "type": "object",
            "properties": {
                "score": { "type": "number", "minimum": -0.5, "maximum": 1, "multipleOf": 0.01 },
            },
            "required": ["score"],
        }));
        for (body, admitted) in [
            (json!({ "score": 0.25 }), true),
            (json!({ "score": 1 }), true),
            (json!({ "score": -0.5 }), true),
            (json!({ "score": 0.125 }), false),
            (json!({ "score": 1.01 }), false),
            (json!({ "score": 0.300_000_000_000_000_04 }), false),
            (json!({ "score": "0.25" }), false),
        ] {
            assert_eq!(
                shape.validate(&body.to_string()).is_ok(),
                admitted,
                "{body} admitted should be {admitted}"
            );
        }
        let normalized = shape.normalized();
        assert_eq!(normalized["properties"]["score"]["minimum"], json!(-0.5));
        assert_eq!(normalized["properties"]["score"]["maximum"], json!(1.0));
        assert_eq!(normalized["properties"]["score"]["multipleOf"], json!(0.01));
        let wire = serde_json::to_string(&shape).expect("a shape serializes");
        let reread: ReturnShape = serde_json::from_str(&wire).expect("the normalized rendering deserializes");
        assert_eq!(reread, shape);
    }

    #[test]
    fn an_unbounded_or_precision_free_number_is_refused() {
        let leaf_cases = [
            (
                json!({ "type": "number", "minimum": 0, "maximum": 1 }),
                ShapeError::BadNumberPrecision,
            ),
            (
                json!({ "type": "number", "minimum": 0, "maximum": 1, "multipleOf": 0.3 }),
                ShapeError::BadNumberPrecision,
            ),
            (
                json!({ "type": "number", "minimum": 0, "maximum": 1, "multipleOf": 1e-7 }),
                ShapeError::BadNumberPrecision,
            ),
            (
                json!({ "type": "number", "minimum": 0.005, "maximum": 1, "multipleOf": 0.01 }),
                ShapeError::BadNumberBounds,
            ),
            (
                json!({ "type": "number", "maximum": 1, "multipleOf": 0.01 }),
                ShapeError::BadNumberBounds,
            ),
            (
                json!({ "type": "number", "minimum": 1, "maximum": 0, "multipleOf": 0.1 }),
                ShapeError::BadNumberBounds,
            ),
            (
                json!({ "type": "number", "minimum": 0, "maximum": 1e17, "multipleOf": 0.000_001 }),
                ShapeError::BadNumberBounds,
            ),
        ];
        for (leaf, expected) in leaf_cases {
            let authored = json!({
                "type": "object",
                "properties": { "score": leaf },
                "required": ["score"],
            });
            assert_eq!(ReturnShape::compile(&authored), Err(expected));
        }
    }
}
