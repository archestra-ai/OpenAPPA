"""Validate the deliberately bounded JSON Schema vocabulary in the pinned capture.

Unknown keywords fail closed. Objects are closed unless the snapshot explicitly
allows additional properties (a deliberate tightening of JSON Schema defaults). This is not a general JSON Schema implementation;
schema drift must be reviewed before broadening its supported vocabulary.
"""
import math
import re
from datetime import datetime

PROFILES = ("approved-writes", "read-only", "team-use", "production-lockdown")

KEYWORDS = {"$schema", "type", "description", "default", "properties", "required", "additionalProperties",
            "items", "minItems", "maxItems", "minLength", "maxLength", "enum", "const", "minimum", "maximum",
            "exclusiveMinimum", "exclusiveMaximum", "pattern", "format", "oneOf", "anyOf", "propertyNames"}


def check_schema(schema):
    if isinstance(schema, bool):
        return
    if not isinstance(schema, dict) or set(schema) - KEYWORDS:
        raise ValueError("unreviewed schema vocabulary")
    for child in schema.get("properties", {}).values():
        check_schema(child)
    for key in ("items", "additionalProperties", "propertyNames"):
        if key in schema:
            check_schema(schema[key])
    for key in ("oneOf", "anyOf"):
        for child in schema.get(key, []):
            check_schema(child)
    if "format" in schema and schema["format"] not in ("uri", "date-time"):
        raise ValueError("unreviewed string format")


def equal(a, b):
    if isinstance(a, bool) != isinstance(b, bool):
        return False
    return a == b


def valid(schema, value):
    if isinstance(schema, bool):
        return schema
    if "oneOf" in schema and sum(valid(s, value) for s in schema["oneOf"]) != 1:
        return False
    if "anyOf" in schema and not any(valid(s, value) for s in schema["anyOf"]):
        return False
    kinds = schema.get("type", [])
    if isinstance(kinds, str):
        kinds = [kinds]
    number = type(value) is int or (type(value) is float and math.isfinite(value))
    fits = {"object": isinstance(value, dict), "array": isinstance(value, list), "string": isinstance(value, str),
            "boolean": type(value) is bool, "null": value is None, "number": number,
            "integer": number and value == int(value)}
    if kinds and not any(fits.get(kind, False) for kind in kinds):
        return False
    if "const" in schema and not equal(value, schema["const"]):
        return False
    if "enum" in schema and not any(equal(value, item) for item in schema["enum"]):
        return False
    if isinstance(value, dict):
        if not set(schema.get("required", [])).issubset(value):
            return False
        properties = schema.get("properties", {})
        additional = schema.get("additionalProperties", not ("object" in kinds or "properties" in schema))
        for key, item in value.items():
            if not isinstance(key, str) or not valid(schema.get("propertyNames", True), key):
                return False
            if not valid(properties.get(key, additional), item):
                return False
    if isinstance(value, list):
        if not schema.get("minItems", 0) <= len(value) <= schema.get("maxItems", math.inf):
            return False
        if not all(valid(schema.get("items", True), item) for item in value):
            return False
    if isinstance(value, str):
        if not schema.get("minLength", 0) <= len(value) <= schema.get("maxLength", math.inf):
            return False
        if "pattern" in schema and not re.search(schema["pattern"], value, re.ASCII):
            return False
        if schema.get("format") == "uri" and not re.fullmatch(r"[A-Za-z][A-Za-z0-9+.-]*:[^\s]+", value):
            return False
        if schema.get("format") == "date-time":
            try:
                if "T" not in value or datetime.fromisoformat(value.replace("Z", "+00:00")).tzinfo is None:
                    return False
            except ValueError:
                return False
    if number:
        for key, passes in (("minimum", lambda x: value >= x), ("maximum", lambda x: value <= x),
                            ("exclusiveMinimum", lambda x: value > x), ("exclusiveMaximum", lambda x: value < x)):
            if key in schema and not passes(schema[key]):
                return False
    return True
