import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parent))
import build
from linear_schema import check_schema, valid


class SchemaTests(unittest.TestCase):
    def test_nullable_objects_unions_and_nested_properties(self):
        schema = {"type": "object", "required": ["patch"], "additionalProperties": False,
                  "properties": {"patch": {"oneOf": [{"type": "null"}, {"type": "object",
                      "additionalProperties": {"type": "string"}, "propertyNames": {"minLength": 1}}]}}}
        for value in ({"patch": None}, {"patch": {"title": "new"}}):
            self.assertTrue(valid(schema, value))
        for value in ({}, {"patch": {}, "extra": 1}, {"patch": {"title": False}}, {"patch": {"": "x"}}):
            self.assertFalse(valid(schema, value))
        self.assertFalse(valid({"oneOf": [{"type": "number"}, {"type": "integer"}]}, 1))

    def test_bounds_and_non_finite_or_boolean_numbers(self):
        schema = {"type": "integer", "minimum": 1, "maximum": 50}
        for value in (True, 0, 51, 1.1, float('inf'), float('nan'), 10**1000):
            self.assertFalse(valid(schema, value))
        self.assertTrue(valid(schema, 50))
        self.assertFalse(valid({"enum": [1]}, True))
        self.assertFalse(valid({"type": "array", "minItems": 1, "items": {"type": "string"}}, []))
        self.assertFalse(valid({"type": "array", "maxItems": 1}, [1, 2]))

    def test_captured_string_constraints(self):
        schema = {"type": "string", "pattern": "^[a-fA-F0-9]{64}$"}
        self.assertTrue(valid(schema, 'a'*64))
        self.assertFalse(valid(schema, 'g'*64))
        self.assertFalse(valid({"type": "string", "format": "uri"}, 'not a URL'))
        self.assertFalse(valid({"type": "string", "format": "date-time"}, '2026-02-30T12:00:00Z'))
        self.assertTrue(valid({"type": "string", "format": "date-time"}, '2026-09-09T12:00:00Z'))

    def test_unknown_vocabulary_and_new_unclassified_fields_refuse_generation(self):
        with self.assertRaises(ValueError):
            check_schema({"type": "object", "properties": {"x": {"$ref": "#"}}})
        original = Path.read_text
        def read(path, *args, **kwargs):
            text = original(path, *args, **kwargs)
            if path.name == 'schemas.json':
                data = json.loads(text)
                data['surfaces']['read-write']['tools'][0]['inputSchema']['properties']['newScope'] = {"type": "string"}
                return json.dumps(data)
            return text
        with patch.object(Path, 'read_text', read), self.assertRaises(ValueError):
            build.render()


if __name__ == '__main__':
    unittest.main()
