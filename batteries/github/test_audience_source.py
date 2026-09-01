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


def fixture_api(responses):
    """A call answering from recorded GitHub REST payloads, in order."""

    remaining = list(responses)

    def call(path, **params):
        for index, (fixture_path, fixture_params, response) in enumerate(remaining):
            if fixture_path == path and fixture_params == params:
                remaining.pop(index)
                if isinstance(response, Exception):
                    raise response
                return response
        raise AssertionError(f"unexpected call {path} {params}")

    return call


def user(login, type="User"):
    return {"login": login, "type": type}


class SelectorTests(unittest.TestCase):
    def test_viewer_carries_the_tokens_own_verified_primary_email(self):
        call = fixture_api(
            [
                ("/user", {}, user("alice")),
                (
                    "/user/emails",
                    {},
                    [
                        {"email": "old@corp.com", "primary": False, "verified": True},
                        {"email": "alice@gmail.com", "primary": True, "verified": True},
                        {"email": "spoof@corp.com", "primary": False, "verified": False},
                    ],
                ),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}),
            {"members": [{"id": "github:alice", "verified_email": "alice@gmail.com"}]},
        )

    def test_a_viewer_without_readable_emails_keeps_the_bare_identity(self):
        call = fixture_api(
            [
                ("/user", {}, user("alice")),
                ("/user/emails", {}, AUDIENCE_SOURCE.Forbidden("/user/emails")),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}),
            {"members": [{"id": "github:alice"}]},
        )

    def test_org_members_are_bare_identities_bots_excluded(self):
        first_page = [user(f"member-{index}") for index in range(100)]
        call = fixture_api(
            [
                ("/orgs/archestra-ai/members", {"per_page": 100, "page": 1}, first_page),
                (
                    "/orgs/archestra-ai/members",
                    {"per_page": 100, "page": 2},
                    [user("alice"), user("ci-robot", type="Bot")],
                ),
            ]
        )
        answer = AUDIENCE_SOURCE.answer(call, {"selector": "org/archestra-ai/members"})
        self.assertEqual(len(answer["members"]), 101)
        self.assertIn({"id": "github:alice"}, answer["members"])
        self.assertNotIn({"id": "github:ci-robot"}, answer["members"])
        for claims in answer["members"]:
            self.assertNotIn("verified_email", claims)

    def test_a_team_reports_its_own_membership(self):
        call = fixture_api(
            [
                (
                    "/orgs/archestra-ai/teams/finance/members",
                    {"per_page": 100, "page": 1},
                    [user("alice"), user("bob")],
                )
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "org/archestra-ai/team/finance"}),
            {"members": [{"id": "github:alice"}, {"id": "github:bob"}]},
        )

    def test_an_unknown_org_or_team_is_a_failure_not_an_empty_answer(self):
        call = fixture_api(
            [
                (
                    "/orgs/typo-org/members",
                    {"per_page": 100, "page": 1},
                    AUDIENCE_SOURCE.NotFound("/orgs/typo-org/members"),
                )
            ]
        )
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            AUDIENCE_SOURCE.answer(call, {"selector": "org/typo-org/members"})

    def test_an_unserved_selector_is_refused(self):
        call = fixture_api([])
        for selector in ["full-members", "org//members", "org/a/team/", "org/a/repos", ""]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"selector": selector})


class MemberLookupTests(unittest.TestCase):
    def test_a_known_member_echoes_the_queried_spelling_never_a_profile_email(self):
        call = fixture_api([("/users/alice", {}, {"login": "Alice", "email": "spoof@corp.com"})])
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"member": "github:alice"}),
            {"claims": {"id": "github:alice"}},
        )

    def test_an_unknown_member_is_a_definitive_null(self):
        call = fixture_api([("/users/ghost", {}, AUDIENCE_SOURCE.NotFound("/users/ghost"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "github:ghost"}), {"claims": None})

    def test_a_foreign_or_bare_member_spelling_is_refused(self):
        call = fixture_api([])
        for member in ["slack:U1", "github:", "alice", ""]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"member": member})


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
            "name": "github",
            "declaration": {"templates": ["viewer", "org/<org>/members", "org/<org>/team/<team>"]},
            "artifact": {"selector": "viewer"},
            **overrides,
        }

    def test_a_foreign_envelope_is_refused(self):
        env = {"PATH": "/usr/bin:/bin", "OPENAPPA_GITHUB_TOKEN": "ghp-fixture"}
        for request in [
            self.envelope(version=2),
            self.envelope(kind="annotation"),
            self.envelope(name="slack"),
        ]:
            result = self.run_script(request, env)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_a_missing_token_is_a_failure_before_any_network(self):
        result = self.run_script(self.envelope(), {"PATH": "/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("OPENAPPA_GITHUB_TOKEN", result.stderr)


if __name__ == "__main__":
    unittest.main()
