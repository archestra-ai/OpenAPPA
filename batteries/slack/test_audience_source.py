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
    """A call answering from recorded Slack Web API payloads, in order."""

    remaining = list(responses)

    def call(method, **params):
        for index, (fixture_method, fixture_params, response) in enumerate(remaining):
            if fixture_method == method and fixture_params == params:
                remaining.pop(index)
                return response
        raise AssertionError(f"unexpected call {method} {params}")

    return call


def user(id, email=None, **flags):
    profile = {"email": email} if email else {}
    return {"id": id, "team_id": flags.pop("team_id", "T1"), "profile": profile, **flags}


class SelectorTests(unittest.TestCase):
    def test_viewer_is_the_tokens_own_principal(self):
        call = fixture_api(
            [
                ("auth.test", {}, {"ok": True, "user_id": "U1", "team_id": "T1"}),
                ("users.info", {"user": "U1"}, {"ok": True, "user": user("U1", "alice@corp.com")}),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}),
            {"members": [{"id": "slack:U1", "verified_email": "alice@corp.com"}]},
        )

    def test_full_members_excludes_guests_connect_bots_and_deleted(self):
        page_one = {
            "ok": True,
            "members": [
                user("U1", "alice@corp.com"),
                user("U2", "guest@other.com", is_restricted=True),
                user("U3", "single@other.com", is_ultra_restricted=True),
                user("U4", "connect@partner.com", is_stranger=True),
                user("U5", "bot@corp.com", is_bot=True),
            ],
            "response_metadata": {"next_cursor": "page-2"},
        }
        page_two = {
            "ok": True,
            "members": [
                user("U6", "app@corp.com", is_app_user=True),
                user("U7", "gone@corp.com", deleted=True),
                user("U8", "foreign@partner.com", team_id="T2"),
                user("USLACKBOT"),
                user("U9"),
            ],
            "response_metadata": {"next_cursor": ""},
        }
        call = fixture_api(
            [
                ("auth.test", {}, {"ok": True, "user_id": "U1", "team_id": "T1"}),
                ("users.list", {"limit": 200}, page_one),
                ("users.list", {"limit": 200, "cursor": "page-2"}, page_two),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "full-members"}),
            {
                "members": [
                    {"id": "slack:U1", "verified_email": "alice@corp.com"},
                    {"id": "slack:U9"},
                ]
            },
        )

    def test_a_user_group_reports_its_own_membership_guests_included(self):
        call = fixture_api(
            [
                (
                    "usergroups.list",
                    {},
                    {"ok": True, "usergroups": [{"id": "S1", "handle": "finance"}, {"id": "S2", "handle": "eng"}]},
                ),
                ("usergroups.users.list", {"usergroup": "S1"}, {"ok": True, "users": ["U1", "U2", "U3"]}),
                ("users.info", {"user": "U1"}, {"ok": True, "user": user("U1", "alice@corp.com")}),
                (
                    "users.info",
                    {"user": "U2"},
                    {"ok": True, "user": user("U2", "auditor@consulting.com", is_restricted=True)},
                ),
                ("users.info", {"user": "U3"}, {"ok": True, "user": user("U3", deleted=True)}),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "user-group/finance"}),
            {
                "members": [
                    {"id": "slack:U1", "verified_email": "alice@corp.com"},
                    {"id": "slack:U2", "verified_email": "auditor@consulting.com"},
                ]
            },
        )

    def test_an_unknown_user_group_handle_is_a_failure_not_an_empty_answer(self):
        call = fixture_api([("usergroups.list", {}, {"ok": True, "usergroups": []})])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "user-group/finance"})

    def test_an_unserved_selector_is_refused(self):
        call = fixture_api([])
        for selector in ["members", "user-group/", "viewer/extra", ""]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"selector": selector})

    def test_a_slack_error_is_a_failure(self):
        call = fixture_api([("auth.test", {}, {"ok": False, "error": "invalid_auth"})])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "viewer"})


class MemberLookupTests(unittest.TestCase):
    def test_a_known_member_reports_its_claims(self):
        call = fixture_api(
            [("users.info", {"user": "U1"}, {"ok": True, "user": user("U1", "alice@corp.com")})]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"member": "slack:U1"}),
            {"claims": {"id": "slack:U1", "verified_email": "alice@corp.com"}},
        )

    def test_a_member_without_an_email_keeps_the_bare_claim(self):
        call = fixture_api([("users.info", {"user": "U9"}, {"ok": True, "user": user("U9")})])
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"member": "slack:U9"}),
            {"claims": {"id": "slack:U9"}},
        )

    def test_an_unknown_member_is_a_definitive_null(self):
        call = fixture_api([("users.info", {"user": "U404"}, {"ok": False, "error": "user_not_found"})])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "slack:U404"}), {"claims": None})

    def test_any_other_lookup_error_is_a_failure(self):
        call = fixture_api([("users.info", {"user": "U1"}, {"ok": False, "error": "ratelimited"})])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"member": "slack:U1"})

    def test_a_foreign_or_bare_member_spelling_is_refused(self):
        call = fixture_api([])
        for member in ["github:alice", "slack:", "U1", ""]:
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
            "name": "slack",
            "declaration": {"templates": ["viewer", "full-members", "user-group/<handle>"]},
            "artifact": {"selector": "viewer"},
            **overrides,
        }

    def test_a_foreign_envelope_is_refused(self):
        env = {"PATH": "/usr/bin:/bin", "APPA_SLACK_TOKEN": "xoxb-fixture"}
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
        self.assertIn("APPA_SLACK_TOKEN", result.stderr)


if __name__ == "__main__":
    unittest.main()
