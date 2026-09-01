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

USERINFO = AUDIENCE_SOURCE.USERINFO_URL
DIRECTORY = AUDIENCE_SOURCE.DIRECTORY_ROOT


def fixture_api(responses):
    """A call answering from recorded Google API payloads, in order."""

    remaining = list(responses)

    def call(url, **params):
        for index, (fixture_url, fixture_params, response) in enumerate(remaining):
            if fixture_url == url and fixture_params == params:
                remaining.pop(index)
                if isinstance(response, Exception):
                    raise response
                return response
        raise AssertionError(f"unexpected call {url} {params}")

    return call


class SelectorTests(unittest.TestCase):
    def test_viewer_carries_its_email_only_when_verified(self):
        call = fixture_api([(USERINFO, {}, {"email": "alice@corp.com", "email_verified": True})])
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}),
            {"members": [{"id": "google-workspace:alice@corp.com", "verified_email": "alice@corp.com"}]},
        )

        call = fixture_api([(USERINFO, {}, {"email": "alice@corp.com", "email_verified": False})])
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}),
            {"members": [{"id": "google-workspace:alice@corp.com"}]},
        )

    def test_full_members_excludes_suspended_and_archived_accounts(self):
        users_url = f"{DIRECTORY}/users"
        page_one = {
            "users": [
                {"primaryEmail": "alice@corp.com"},
                {"primaryEmail": "gone@corp.com", "suspended": True},
            ],
            "nextPageToken": "page-2",
        }
        page_two = {
            "users": [
                {"primaryEmail": "old@corp.com", "archived": True},
                {"primaryEmail": "bob@corp.com"},
            ]
        }
        call = fixture_api(
            [
                (users_url, {"customer": "my_customer", "maxResults": 500}, page_one),
                (users_url, {"customer": "my_customer", "maxResults": 500, "pageToken": "page-2"}, page_two),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "full-members"}),
            {
                "members": [
                    {"id": "google-workspace:alice@corp.com", "verified_email": "alice@corp.com"},
                    {"id": "google-workspace:bob@corp.com", "verified_email": "bob@corp.com"},
                ]
            },
        )

    def test_a_group_expands_nested_groups_and_keeps_external_members(self):
        finance_url = f"{DIRECTORY}/groups/finance%40corp.com/members"
        leads_url = f"{DIRECTORY}/groups/leads%40corp.com/members"
        call = fixture_api(
            [
                (
                    finance_url,
                    {"maxResults": 200},
                    {
                        "members": [
                            {"type": "USER", "email": "alice@corp.com", "status": "ACTIVE"},
                            {"type": "EXTERNAL", "email": "auditor@consulting.com"},
                            {"type": "USER", "email": "gone@corp.com", "status": "SUSPENDED"},
                            {"type": "GROUP", "email": "leads@corp.com"},
                        ]
                    },
                ),
                (
                    leads_url,
                    {"maxResults": 200},
                    {
                        "members": [
                            {"type": "USER", "email": "bob@corp.com", "status": "ACTIVE"},
                            {"type": "USER", "email": "alice@corp.com", "status": "ACTIVE"},
                            {"type": "GROUP", "email": "finance@corp.com"},
                        ]
                    },
                ),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "group/finance@corp.com"}),
            {
                "members": [
                    {"id": "google-workspace:alice@corp.com", "verified_email": "alice@corp.com"},
                    # An external member belongs to the group but the
                    # Workspace attests no account for that address.
                    {"id": "google-workspace:auditor@consulting.com"},
                    {"id": "google-workspace:bob@corp.com", "verified_email": "bob@corp.com"},
                ]
            },
        )

    def test_an_unexpandable_group_member_is_a_failure_not_an_under_report(self):
        url = f"{DIRECTORY}/groups/everyone%40corp.com/members"
        call = fixture_api([(url, {"maxResults": 200}, {"members": [{"type": "CUSTOMER", "id": "C123"}]})])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "group/everyone@corp.com"})

    def test_an_unknown_group_is_a_failure_not_an_empty_answer(self):
        url = f"{DIRECTORY}/groups/typo%40corp.com/members"
        call = fixture_api([(url, {"maxResults": 200}, AUDIENCE_SOURCE.NotFound(url))])
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            AUDIENCE_SOURCE.answer(call, {"selector": "group/typo@corp.com"})

    def test_an_unserved_selector_is_refused(self):
        call = fixture_api([])
        for selector in ["members", "group/", "viewer/extra", ""]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"selector": selector})


class MemberLookupTests(unittest.TestCase):
    def test_a_known_member_echoes_the_queried_spelling_with_its_primary_address(self):
        url = f"{DIRECTORY}/users/alias%40corp.com"
        call = fixture_api([(url, {}, {"primaryEmail": "alice@corp.com"})])
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"member": "google-workspace:alias@corp.com"}),
            {"claims": {"id": "google-workspace:alias@corp.com", "verified_email": "alice@corp.com"}},
        )

    def test_an_unknown_member_is_a_definitive_null(self):
        url = f"{DIRECTORY}/users/ghost%40corp.com"
        call = fixture_api([(url, {}, AUDIENCE_SOURCE.NotFound(url))])
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"member": "google-workspace:ghost@corp.com"}),
            {"claims": None},
        )

    def test_a_foreign_or_bare_member_spelling_is_refused(self):
        call = fixture_api([])
        for member in ["slack:U1", "google-workspace:", "alice@corp.com", ""]:
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
            "name": "google-workspace",
            "declaration": {"templates": ["viewer", "full-members", "group/<group-address>"]},
            "artifact": {"selector": "viewer"},
            **overrides,
        }

    def test_a_foreign_envelope_is_refused(self):
        env = {"PATH": "/usr/bin:/bin", "OPENAPPA_GOOGLE_WORKSPACE_TOKEN": "ya29-fixture"}
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
        self.assertIn("OPENAPPA_GOOGLE_WORKSPACE_TOKEN", result.stderr)


if __name__ == "__main__":
    unittest.main()
