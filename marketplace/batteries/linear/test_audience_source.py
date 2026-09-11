import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest


SCRIPT = Path(__file__).with_name("audience-source.py")
SPEC = importlib.util.spec_from_file_location("audience_source", SCRIPT)
AUDIENCE_SOURCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIENCE_SOURCE)


def no_api(*_args, **_kwargs):
    raise AssertionError("Linear must not be asked")


def user(id, email, **flags):
    return {"id": id, "email": email, "active": True, "guest": False, "app": False, **flags}


class ShapingTests(unittest.TestCase):
    def test_a_roster_over_the_bound_is_refused_before_it_is_read_to_the_end(self):
        pages = []

        def call(_query, **variables):
            pages.append(variables["after"])
            nodes = [user(f"u{len(pages)}-{i}", f"u{len(pages)}-{i}@corp.com") for i in range(250)]
            return {"users": {"nodes": nodes, "pageInfo": {"hasNextPage": True, "endCursor": f"c{len(pages)}"}}}

        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.full_members(call)
        self.assertEqual(len(pages), AUDIENCE_SOURCE.MAX_NODES // 250 + 1)

    def test_a_member_is_the_reported_email_else_the_qualified_id(self):
        self.assertEqual(AUDIENCE_SOURCE.reader_of(user("u1", "alice@corp.com")), "alice@corp.com")
        self.assertEqual(AUDIENCE_SOURCE.reader_of({"id": "u2", "email": None}), "linear:u2")

    def test_app_users_and_deactivated_accounts_are_not_people(self):
        self.assertTrue(AUDIENCE_SOURCE.is_person(user("u1", "alice@corp.com")))
        self.assertTrue(AUDIENCE_SOURCE.is_person(user("u1", "guest@other.com", guest=True)))
        self.assertFalse(AUDIENCE_SOURCE.is_person(user("u3", "bot@linear.linear.app", app=True)))
        self.assertFalse(AUDIENCE_SOURCE.is_person(user("u4", "gone@corp.com", active=False)))

    def test_a_union_keeps_every_reader_once_in_first_seen_order(self):
        self.assertEqual(
            AUDIENCE_SOURCE.union(["a@x", "b@x"], ["b@x", "c@x"], ["a@x"]),
            ["a@x", "b@x", "c@x"],
        )


class RefusalTests(unittest.TestCase):
    def test_an_unserved_selector_is_refused_before_linear_is_asked(self):
        for selector in [
            "",
            "members",
            "team//members",
            "team/ENG",
            "team/ENG/issues",
            "issue//readers",
            "issue/ENG-1/members",
            "project/x/readers/extra",
            "document/",
            "viewer/extra",
        ]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(no_api, {"selector": selector})

    def test_a_foreign_or_bare_member_spelling_is_refused(self):
        for member in ["github:alice", "linear:", "linear:not-a-uuid", "alice@corp.com", ""]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(no_api, {"member": member})

    def test_a_malformed_artifact_is_refused(self):
        for artifact in [None, [], {}, {"selector": "viewer", "member": "linear:x"}, {"other": 1}]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(no_api, artifact)

    def test_a_team_of_unknown_visibility_is_a_failure_not_a_guess(self):
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.team_readers(no_api, {"id": "t1", "visibility": "secret"})


class EnvelopeTests(unittest.TestCase):
    def run_script(self, request, env):
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=json.dumps(request),
            capture_output=True,
            text=True,
            env=env,
        )

    def envelope(self, **overrides):
        return {
            "version": 1,
            "kind": "audience",
            "name": "linear",
            "declaration": {"templates": list(AUDIENCE_SOURCE.SERVED_TEMPLATES)},
            "artifact": {"selector": "viewer"},
            **overrides,
        }

    def test_a_foreign_envelope_is_refused(self):
        env = {"PATH": "/usr/bin:/bin", "APPA_PROVIDER_LINEAR_TOKEN": "lin_api_fixture"}
        for request in [
            self.envelope(version=2),
            self.envelope(kind="annotation"),
            self.envelope(name="github"),
        ]:
            result = self.run_script(request, env)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_a_missing_token_is_a_failure_before_any_network(self):
        result = self.run_script(self.envelope(), {"PATH": "/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("APPA_PROVIDER_LINEAR_TOKEN", result.stderr)

    def test_a_foreign_declaration_is_refused_before_the_token_is_read(self):
        declared = self.envelope()["declaration"]["templates"]
        for templates in [declared + ["foreign/<x>"], declared[1:], [], list(reversed(declared))]:
            result = self.run_script(self.envelope(declaration={"templates": templates}), {"PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
