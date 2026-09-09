import importlib.util
import json
import re
import shutil
import subprocess
import tempfile
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
    def test_generated_profiles_apply_the_expected_write_requirements(self):
        profiles = [re.search(r'^name = "linear\.([^"\n]+)"$', body, re.MULTILINE).group(1)
                    for body in build.render().values()]
        self.assertEqual(set(profiles), {"read-only", "approved-writes", "team-use", "production-lockdown"})
        for profile in profiles:
            with self.subTest(profile=profile):
                req = request("save_comment", {"issueId": "ENG-1", "body": "update"}, profile, production=True)
                if profile == "read-only":
                    with self.assertRaises(module.Refusal):
                        module.annotate(req)
                else:
                    answer = module.annotate(req)["answer"]
                    self.assertEqual(answer["emits"], ["linear.changed"])
                    self.assertEqual(answer["requires"]["trust"], "trusted")
                    self.assertEqual(answer["requires"]["attention"], [] if profile == "team-use" else ["linear-review"])

    def test_relocated_helper_runs_without_mutating_its_package(self):
        with tempfile.TemporaryDirectory() as directory:
            package = Path(directory) / "linear"
            shutil.copytree(Path(__file__).parent, package, ignore=shutil.ignore_patterns("__pycache__"))
            result = subprocess.run([sys.executable, str(package / "annotate.py")],
                                    input=json.dumps(request("get_issue", {"id": "ENG-1"})),
                                    cwd=directory, capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["answer"]["delta"]["audience"], ["alice@corp.example"])
            self.assertFalse((package / "__pycache__").exists())

    def test_unknown_nested_patch_fields_cannot_bypass_scope_review(self):
        args = {"id": "ENG-1", "patch": [{"op": "append", "text": "update", "team": "outside"}]}
        with self.assertRaises(module.Refusal):
            module.annotate(request("save_issue", args, match={"id": "ENG-1"}))

    def test_text_patch_can_vary_without_changing_destination(self):
        args = {"id": "ENG-1", "patch": [{"op": "append", "text": "update"}]}
        self.assertEqual(module.annotate(request("save_issue", args, match={"id": "ENG-1"}))["answer"]["requires"]["audience"],
                         {"contains": ["alice@corp.example"]})

    def test_variable_content_cannot_hide_structured_scope_changes(self):
        for body in [{"team": "outside"}, [{"team": "outside"}]]:
            with self.subTest(body=body), self.assertRaises(module.Refusal):
                module.annotate(request("save_comment", {"issueId": "ENG-1", "body": body}, match={"issueId": "ENG-1"}))

    def test_provider_format_validation_is_left_to_provider(self):
        args = {"issueId": "ENG-1", "body": ""}
        module.annotate(request("save_comment", args, match={"issueId": "ENG-1"}))
        with self.assertRaises(module.Refusal):
            module.annotate(request("get_issue", {}))

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
