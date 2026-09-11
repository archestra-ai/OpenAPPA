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
    confirmed = {"is_email_confirmed": True} if email else {}
    return {
        "id": id,
        "team_id": flags.pop("team_id", "T1"),
        "profile": profile,
        **confirmed,
        **flags,
    }


class SelectorTests(unittest.TestCase):
    def test_viewer_is_the_tokens_own_confirmed_email(self):
        call = fixture_api(
            [
                ("auth.test", {}, {"ok": True, "user_id": "U1", "team_id": "T1"}),
                ("users.info", {"user": "U1"}, {"ok": True, "user": user("U1", "alice@corp.com")}),
            ]
        )
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}), {"members": ["alice@corp.com"]})

    def test_an_unconfirmed_profile_address_leaves_the_qualified_id(self):
        call = fixture_api(
            [
                ("auth.test", {}, {"ok": True, "user_id": "U1", "team_id": "T1"}),
                (
                    "users.info",
                    {"user": "U1"},
                    {
                        "ok": True,
                        "user": {
                            "id": "U1",
                            "team_id": "T1",
                            "profile": {"email": "alice@corp.com"},
                            "is_email_confirmed": False,
                        },
                    },
                ),
            ]
        )
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}), {"members": ["slack:U1"]})

    def test_a_directory_over_the_bound_is_refused(self):
        pages = [
            (
                "users.list",
                {"limit": 200, **({"cursor": f"page-{page}"} if page else {})},
                {
                    "ok": True,
                    "members": [user(f"U{page}-{i}", f"u{page}-{i}@corp.com") for i in range(200)],
                    "response_metadata": {"next_cursor": f"page-{page + 1}"},
                },
            )
            for page in range(27)
        ]
        call = fixture_api([("auth.test", {}, {"ok": True, "user_id": "U1", "team_id": "T1"}), *pages])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "full-members"})

    def test_a_page_without_members_is_a_failure_not_a_partial_answer(self):
        call = fixture_api(
            [
                ("auth.test", {}, {"ok": True, "user_id": "U1", "team_id": "T1"}),
                ("users.list", {"limit": 200}, {"ok": True}),
            ]
        )
        with self.assertRaises(KeyError):
            AUDIENCE_SOURCE.answer(call, {"selector": "full-members"})

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
                {"id": "U0", "profile": {}},
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
            {"members": ["alice@corp.com", "slack:U9"]},
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
                (
                    "users.list",
                    {"limit": 200},
                    {
                        "ok": True,
                        "members": [
                            user("U1", "alice@corp.com"),
                            user("U2", "auditor@consulting.com", is_restricted=True),
                            user("U3", deleted=True),
                            user("U4", "unrelated@corp.com"),
                        ],
                    },
                ),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "user-group/finance"}),
            {"members": ["alice@corp.com", "auditor@consulting.com"]},
        )

    def test_an_unknown_user_group_handle_is_a_failure_not_an_empty_answer(self):
        call = fixture_api([("usergroups.list", {}, {"ok": True, "usergroups": []})])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "user-group/finance"})

    def test_an_unserved_selector_is_refused(self):
        call = fixture_api([])
        for selector in ["members", "user-group/", "viewer/extra", "", "channel/", "channel/C1/extra"]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"selector": selector})

    def test_a_channel_id_names_a_conversation_and_a_user_id_names_a_dm(self):
        for channel_id in ["C0ABC123", "G0ABC123", "D0ABC123"]:
            self.assertEqual(AUDIENCE_SOURCE.conversation_kind(channel_id), "conversation")
        for channel_id in ["U0ABC123", "W0ABC123"]:
            self.assertEqual(AUDIENCE_SOURCE.conversation_kind(channel_id), "user")

    def test_a_public_channel_is_read_by_every_full_member_and_whoever_is_in_it(self):
        call = fixture_api(
            [
                ("conversations.info", {"channel": "C1"}, {"ok": True, "channel": {"id": "C1", "is_private": False}}),
                ("conversations.members", {"channel": "C1", "limit": 200}, {"ok": True, "members": ["U1", "U3"]}),
                ("auth.test", {}, {"ok": True, "user_id": "U1", "team_id": "T1"}),
                (
                    "users.list",
                    {"limit": 200},
                    {
                        "ok": True,
                        "members": [
                            user("U1", "alice@corp.com"),
                            user("U2", "bob@corp.com"),
                            user("U3", "guest@other.com", is_restricted=True),
                            user("U4", "other-guest@other.com", is_restricted=True),
                        ],
                    },
                ),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "channel/C1"}),
            {"members": ["alice@corp.com", "bob@corp.com", "guest@other.com"]},
        )

    def test_a_private_conversation_is_read_by_its_members_looked_up_one_by_one(self):
        for conversation in [{"is_private": True}, {"is_im": True}, {"is_mpim": True}]:
            call = fixture_api(
                [
                    ("conversations.info", {"channel": "G1"}, {"ok": True, "channel": {"id": "G1", **conversation}}),
                    ("conversations.members", {"channel": "G1", "limit": 200}, {"ok": True, "members": ["U1", "U7"]}),
                    ("users.info", {"user": "U1"}, {"ok": True, "user": user("U1", "alice@corp.com")}),
                    ("users.info", {"user": "U7"}, {"ok": True, "user": user("U7", "gone@corp.com", deleted=True)}),
                ]
            )
            self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "channel/G1"}), {"members": ["alice@corp.com"]})

    def test_a_conversation_without_visibility_flags_is_refused_not_read_as_public(self):
        for channel in [{"id": "C1"}, {"id": "C1", "is_private": "no"}]:
            call = fixture_api(
                [
                    ("conversations.info", {"channel": "C1"}, {"ok": True, "channel": channel}),
                    ("conversations.members", {"channel": "C1", "limit": 200}, {"ok": True, "members": ["U1"]}),
                ]
            )
            with self.assertRaises(RuntimeError):
                AUDIENCE_SOURCE.answer(call, {"selector": "channel/C1"})

    def test_a_member_the_directory_does_not_report_is_a_failure(self):
        call = fixture_api(
            [
                ("conversations.info", {"channel": "C1"}, {"ok": True, "channel": {"id": "C1", "is_private": False}}),
                ("conversations.members", {"channel": "C1", "limit": 200}, {"ok": True, "members": ["U1", "U9"]}),
                ("auth.test", {}, {"ok": True, "user_id": "U1", "team_id": "T1"}),
                ("users.list", {"limit": 200}, {"ok": True, "members": [user("U1", "alice@corp.com")]}),
            ]
        )
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "channel/C1"})

    def test_anything_but_a_conversation_or_user_id_is_refused_before_slack_is_asked(self):
        call = fixture_api([])
        for channel_id in ["general", "#general", "C", "U", "B0ABC123", "T0ABC123", "https://x.slack.com/archives/C1"]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"selector": f"channel/{channel_id}"})

    def test_a_slack_error_is_a_failure(self):
        call = fixture_api([("auth.test", {}, {"ok": False, "error": "invalid_auth"})])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "viewer"})


class MemberLookupTests(unittest.TestCase):
    def test_a_member_with_a_confirmed_email_resolves_to_it(self):
        call = fixture_api(
            [("users.info", {"user": "U1"}, {"ok": True, "user": user("U1", "alice@corp.com")})]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"member": "slack:U1"}),
            {"principal": "alice@corp.com"},
        )

    def test_a_member_without_an_email_is_the_reader_as_written(self):
        call = fixture_api([("users.info", {"user": "U9"}, {"ok": True, "user": user("U9")})])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "slack:U9"}), {"principal": "slack:U9"})

    def test_an_unknown_member_is_a_definitive_null(self):
        call = fixture_api([("users.info", {"user": "U404"}, {"ok": False, "error": "user_not_found"})])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "slack:U404"}), {"principal": None})

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
            "declaration": {"templates": ["viewer", "full-members", "user-group/<handle>", "channel/<id>"]},
            "artifact": {"selector": "viewer"},
            **overrides,
        }

    def test_a_foreign_envelope_is_refused(self):
        env = {"PATH": "/usr/bin:/bin", "APPA_PROVIDER_SLACK_TOKEN": "xoxb-fixture"}
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
        self.assertIn("APPA_PROVIDER_SLACK_TOKEN", result.stderr)

    def test_a_foreign_declaration_is_refused_before_the_token_is_read(self):
        declared = self.envelope()["declaration"]["templates"]
        for templates in [declared + ["foreign/<x>"], declared[1:], [], list(reversed(declared))]:
            result = self.run_script(self.envelope(declaration={"templates": templates}), {"PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
