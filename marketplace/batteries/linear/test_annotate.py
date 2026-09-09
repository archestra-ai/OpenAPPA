import importlib.util
import json
import re
from pathlib import Path
import unittest
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
import build

SPEC = importlib.util.spec_from_file_location("linear_annotate", Path(__file__).with_name("annotate.py"))
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


def request(tool, arguments, profile="approved-writes", match=None, production=False, audience=None):
    rule = {"match": arguments if match is None else match, "audience": audience or ["alice@corp.example"], "production": production}
    return {"version": 1, "kind": "annotation", "name": "linear." + profile,
            "declaration": {"hint": json.dumps({"rules": {tool: [rule]}})},
            "artifact": {"args": {"name": "mcp/linear/" + tool, "arguments": arguments}}}


class AnnotationTests(unittest.TestCase):
    def test_every_captured_operation_is_classified_and_profiled(self):
        # These are mechanical contract-coverage cases, not provider execution.
        # Exercise the names actually emitted by the generator, so a generated
        # profile cannot escape consult coverage when the profile list changes.
        profiles = [re.search(r'^name = "linear\.([^"\n]+)"$', body, re.MULTILINE).group(1)
                    for body in build.render().values()]
        for tool, schema in module.SCHEMAS.items():
            def sample(s):
                if "const" in s: return s["const"]
                if "enum" in s: return s["enum"][0]
                if "oneOf" in s: return sample(s["oneOf"][0])
                if "anyOf" in s: return sample(s["anyOf"][0])
                kind = s.get("type")
                if isinstance(kind, list): kind = kind[0]
                if kind == "object": return {k:sample(s["properties"][k]) for k in s.get("required", [])}
                if kind == "array": return [sample(s["items"]) for _ in range(s.get("minItems",0))]
                if kind in ("number", "integer"): return max(s.get("minimum",1),s.get("exclusiveMinimum",0)+1)
                if kind == "boolean": return False
                if kind == "null": return None
                if s.get("pattern") == "^[a-fA-F0-9]{64}$": return "a"*64
                if s.get("format") == "uri": return "https://example.com/asset"
                if s.get("format") == "date-time": return "2026-09-09T12:00:00Z"
                return "fixture"
            args = sample(schema)
            for profile in profiles:
                with self.subTest(tool=tool, profile=profile):
                    mutation = module.OPERATIONS[tool]["kind"] != "read"
                    req = request(tool, args, profile, production=True)
                    if mutation and profile == "read-only":
                        with self.assertRaises(module.Refusal):
                            module.annotate(req)
                    else:
                        answer = module.annotate(req)["answer"]
                        self.assertEqual(answer["delta"]["trust"], "suspicious")
                        self.assertEqual(bool(answer["emits"]), mutation)
                        if mutation and profile != "team-use":
                            self.assertEqual(answer["requires"]["attention"], ["linear-review"])

    def test_unknown_nested_patch_fields_cannot_bypass_scope_review(self):
        args = {"id": "ENG-1", "patch": [{"op": "append", "text": "update", "team": "outside"}]}
        with self.assertRaises(module.Refusal):
            module.annotate(request("save_issue", args, match={"id": "ENG-1"}))

    def test_reparent_cannot_reuse_old_issue_mapping(self):
        with self.assertRaises(module.Refusal):
            module.annotate(request("save_issue", {"id": "ENG-1", "team": "outside", "title": "x"}, match={"id": "ENG-1"}))

    def test_related_content_expansion_requires_operator_mapping(self):
        with self.assertRaises(module.Refusal):
            module.annotate(request("get_issue", {"id": "ENG-1", "includeRelations": True}, match={"id": "ENG-1"}))

    def test_write_content_is_variable_but_destination_is_bound(self):
        result = module.annotate(request("save_comment", {"issueId": "ENG-1", "body": "New text"}, match={"issueId": "ENG-1"}))
        self.assertEqual(result["answer"]["requires"]["audience"], {"contains": ["alice@corp.example"]})
        self.assertEqual(result["answer"]["requires"]["trust"], "trusted")

    def test_sensitive_operation_and_delegation_require_review_in_team_mode(self):
        for tool, args in [("merge_diff", {"urlOrId": "PR-1"}), ("share_issue", {"issue": "ENG-1", "user": "u"}),
                           ("save_issue", {"id": "ENG-1", "delegate": "agent"})]:
            with self.subTest(tool=tool):
                self.assertEqual(module.annotate(request(tool, args, "team-use"))["answer"]["requires"]["attention"], ["linear-review"])

    def test_production_needs_explicit_resource_opt_in(self):
        req = request("save_issue", {"id": "ENG-1", "title": "x"}, "production-lockdown")
        with self.assertRaises(module.Refusal):
            module.annotate(req)

    def test_ambiguous_or_missing_mapping_refuses(self):
        req = request("get_issue", {"id": "ENG-1"})
        config = json.loads(req["declaration"]["hint"])
        config["rules"]["get_issue"] *= 2
        req["declaration"]["hint"] = json.dumps(config)
        with self.assertRaises(module.Refusal):
            module.annotate(req)
        req["declaration"]["hint"] = '{"rules":{}}'
        with self.assertRaises(module.Refusal):
            module.annotate(req)

    def test_exact_matching_does_not_coerce_types_or_partial_arrays(self):
        for args, match in [({"id": "ENG-1", "includeRelations": True}, {"id": "ENG-1", "includeRelations": 1}),
                            ({"id": "P-1", "addTeams": ["a", "b"]}, {"id": "P-1", "addTeams": ["a"]})]:
            with self.assertRaises(module.Refusal):
                module.annotate(request("save_project" if "addTeams" in args else "get_issue", args, match=match))

    def test_external_image_fetch_requires_public_input_even_for_private_result(self):
        result = module.annotate(request("extract_images", {"markdown": "![x](https://elsewhere.example/image)"}))["answer"]
        self.assertEqual(result["requires"]["audience"], {"contains": "public"})
        self.assertEqual(result["requires"]["attention"], ["linear-review"])

    def test_host_server_alias_is_resolved_by_runtime_not_guessed_by_helper(self):
        req = request("get_issue", {"id": "ENG-1"})
        expected = module.annotate(req)
        req["artifact"]["args"]["name"] = "mcp/server-123/get_issue"
        self.assertEqual(module.annotate(req), expected)

    def test_upload_signed_url_stays_self(self):
        result = module.annotate(request("prepare_attachment_upload", {"issue": "ENG-1", "filename": "x", "contentType": "text/plain", "size": 1}))["answer"]
        self.assertEqual(result["delta"]["audience"], ["self"])

    def test_unknown_args_tools_and_malformed_names_refuse(self):
        req = request("get_issue", {"id": "ENG-1", "newScope": "outside"})
        with self.assertRaises(module.Refusal):
            module.annotate(req)
        req = request("get_issue", {"id": "ENG-1"})
        req["artifact"]["args"]["name"] = "host/linear/get_issue"
        with self.assertRaises(module.Refusal):
            module.annotate(req)


if __name__ == "__main__":
    unittest.main()
