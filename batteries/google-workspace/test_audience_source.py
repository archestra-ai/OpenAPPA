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


def empty_directory():
    """A directory pass reporting no administered account."""
    return (f"{DIRECTORY}/users", {"customer": "my_customer", "maxResults": 500}, {"users": []})


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
                    f"{DIRECTORY}/users",
                    {"customer": "my_customer", "maxResults": 500},
                    {"users": [{"primaryEmail": "alice@corp.com"}, {"primaryEmail": "bob@corp.com"}]},
                ),
                (
                    finance_url,
                    {"maxResults": 200},
                    {
                        "members": [
                            {"type": "USER", "email": "alice@corp.com", "status": "ACTIVE"},
                            # Google's own EXTERNAL type is documented as unused,
                            # so an outside member arrives as an ordinary USER.
                            {"type": "USER", "email": "auditor@consulting.com"},
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
                    # The auditor belongs to the group, but no directory
                    # account administers that address, so nothing is attested.
                    {"id": "google-workspace:auditor@consulting.com"},
                    {"id": "google-workspace:bob@corp.com", "verified_email": "bob@corp.com"},
                ]
            },
        )

    def test_a_group_member_the_directory_does_not_administer_is_never_attested(self):
        # Same address, same member type, two Workspaces: only the one that
        # administers the account attests it.
        url = f"{DIRECTORY}/groups/finance%40corp.com/members"
        listing = {"members": [{"type": "USER", "email": "alice@corp.com"}]}

        outside = fixture_api([empty_directory(), (url, {"maxResults": 200}, listing)])
        self.assertEqual(
            AUDIENCE_SOURCE.answer(outside, {"selector": "group/finance@corp.com"}),
            {"members": [{"id": "google-workspace:alice@corp.com"}]},
        )

        administered = fixture_api(
            [
                (
                    f"{DIRECTORY}/users",
                    {"customer": "my_customer", "maxResults": 500},
                    {"users": [{"primaryEmail": "alice@corp.com"}]},
                ),
                (url, {"maxResults": 200}, listing),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(administered, {"selector": "group/finance@corp.com"}),
            {"members": [{"id": "google-workspace:alice@corp.com", "verified_email": "alice@corp.com"}]},
        )

    def test_a_group_member_the_directory_administers_is_attested_however_the_account_stands(self):
        # Suspended memberships leave the group, but an archived account is one
        # the Workspace still administers: a direct member lookup attests that
        # address, so the group expansion must not disagree with itself.
        url = f"{DIRECTORY}/groups/finance%40corp.com/members"
        call = fixture_api(
            [
                (
                    f"{DIRECTORY}/users",
                    {"customer": "my_customer", "maxResults": 500},
                    {"users": [{"primaryEmail": "old@corp.com", "archived": True}]},
                ),
                (url, {"maxResults": 200}, {"members": [{"type": "USER", "email": "old@corp.com"}]}),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "group/finance@corp.com"}),
            {"members": [{"id": "google-workspace:old@corp.com", "verified_email": "old@corp.com"}]},
        )

    def test_an_unexpandable_group_member_is_a_failure_not_an_under_report(self):
        # No directory fixture: the traversal fails before anything needs
        # attesting, so the tenant-wide pass is never paid for.
        url = f"{DIRECTORY}/groups/everyone%40corp.com/members"
        call = fixture_api([(url, {"maxResults": 200}, {"members": [{"type": "CUSTOMER", "id": "C123"}]})])
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(call, {"selector": "group/everyone@corp.com"})

    def test_an_unknown_group_is_a_failure_not_an_empty_answer(self):
        url = f"{DIRECTORY}/groups/typo%40corp.com/members"
        call = fixture_api([(url, {"maxResults": 200}, AUDIENCE_SOURCE.NotFound(url))])
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            AUDIENCE_SOURCE.answer(call, {"selector": "group/typo@corp.com"})

    def test_an_empty_group_answers_without_a_directory_pass(self):
        url = f"{DIRECTORY}/groups/empty%40corp.com/members"
        call = fixture_api([(url, {"maxResults": 200}, {"members": []})])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "group/empty@corp.com"}), {"members": []})

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
        env = {"PATH": "/usr/bin:/bin", "APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN": "ya29-fixture"}
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
        self.assertIn("APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN", result.stderr)


if __name__ == "__main__":
    unittest.main()
