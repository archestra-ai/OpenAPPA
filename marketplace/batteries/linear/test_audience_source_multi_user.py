import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest


sys.path.insert(0, str(Path(__file__).parent))
SCRIPT = Path(__file__).with_name("audience-source-multi-user.py")
SPEC = importlib.util.spec_from_file_location("audience_source_multi_user", SCRIPT)
MULTI_USER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MULTI_USER)

SINGLE_USER_TEMPLATES = [
    "viewer",
    "full-members",
    "team/<key>/members",
    "team/<key>/readers",
    "issue/<id>/readers",
    "project/<id>/readers",
    "document/<id>/readers",
]


def no_api(*_args, **_kwargs):
    raise AssertionError("Linear must not be asked")


def run_script(request, env):
    return subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(request), capture_output=True, text=True, env=env)


def envelope(**overrides):
    return {
        "version": 1,
        "kind": "audience",
        "name": "linear",
        "declaration": {"templates": list(MULTI_USER.SERVED_TEMPLATES)},
        "artifact": {"selector": "full-members"},
        **overrides,
    }


class Selectors(unittest.TestCase):
    def test_the_viewer_is_refused_before_linear_is_asked(self):
        with self.assertRaises(ValueError):
            MULTI_USER.answer(no_api, {"selector": "viewer"})

    def test_the_served_templates_are_the_single_user_ones_but_the_viewer(self):
        self.assertEqual(
            MULTI_USER.SERVED_TEMPLATES,
            [template for template in SINGLE_USER_TEMPLATES if template != "viewer"],
        )

    def test_a_team_reads_as_in_the_single_user_source_by_email(self):
        def call(query, **variables):
            if "teams(filter" in query:
                return {"teams": {"nodes": [{"id": "t1", "key": "ENG", "visibility": "private", "parent": None}]}}
            nodes = [
                {"id": "u1", "email": "ana@corp.example", "active": True, "guest": False, "app": False},
                {"id": "u2", "email": "bot@linear.example", "active": True, "guest": False, "app": True},
            ]
            return {"team": {"members": {"nodes": nodes, "pageInfo": {"hasNextPage": False, "endCursor": None}}}}

        self.assertEqual(
            MULTI_USER.answer(call, {"selector": "team/ENG/readers"}),
            {"members": ["ana@corp.example"]},
        )


class Token(unittest.TestCase):
    def test_an_app_token_goes_as_a_bearer_token(self):
        self.assertEqual(MULTI_USER.app_token("lin_oauth_fixture"), "Bearer lin_oauth_fixture")

    def test_a_personal_api_key_is_refused(self):
        with self.assertRaises(ValueError):
            MULTI_USER.app_token("lin_api_fixture")

    def test_a_missing_token_is_a_failure(self):
        result = run_script(envelope(), {"PATH": "/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")


class Declaration(unittest.TestCase):
    def test_a_declaration_with_the_viewer_is_refused_before_the_token_is_read(self):
        declared = SINGLE_USER_TEMPLATES
        result = run_script(envelope(declaration={"templates": declared}), {"PATH": "/usr/bin:/bin"})
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
