"""Cross-battery acceptance: the shipped audience-source scripts produce
member claims whose verified emails drive the engine's verified-email
identity rule — same attested address, same principal; different or
absent address, distinct identities. The engine-side normalization is
tested in appa-engine; these tests pin the claims the real scripts
emit from recorded provider payloads, with no network.
"""

import importlib.util
from pathlib import Path
import unittest


def load_script(battery):
    script = Path(__file__).parent / battery / "audience-source.py"
    spec = importlib.util.spec_from_file_location(f"{battery.replace('-', '_')}_audience_source", script)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SLACK = load_script("slack")
GITHUB = load_script("github")
WORKSPACE = load_script("google-workspace")


def fixture_api(responses):
    remaining = list(responses)

    def call(first, **params):
        for index, (fixture_first, fixture_params, response) in enumerate(remaining):
            if fixture_first == first and fixture_params == params:
                remaining.pop(index)
                if isinstance(response, Exception):
                    raise response
                return response
        raise AssertionError(f"unexpected call {first} {params}")

    return call


class CrossBatteryIdentityTests(unittest.TestCase):
    def test_workspace_and_slack_attest_the_same_corporate_address(self):
        workspace = WORKSPACE.answer(
            fixture_api([(WORKSPACE.USERINFO_URL, {}, {"email": "alice@corp.com", "email_verified": True})]),
            {"selector": "viewer"},
        )["members"][0]
        slack = SLACK.answer(
            fixture_api(
                [
                    ("auth.test", {}, {"ok": True, "user_id": "U012345", "team_id": "T1"}),
                    (
                        "users.info",
                        {"user": "U012345"},
                        {"ok": True, "user": {"id": "U012345", "profile": {"email": "alice@corp.com"}}},
                    ),
                ]
            ),
            {"selector": "viewer"},
        )["members"][0]

        self.assertNotEqual(workspace["id"], slack["id"])
        self.assertEqual(workspace["verified_email"], slack["verified_email"])

    def test_a_personal_github_address_stays_distinct_from_the_corporate_one(self):
        github = GITHUB.answer(
            fixture_api(
                [
                    ("/user", {}, {"login": "alice", "type": "User"}),
                    ("/user/emails", {}, [{"email": "alice@gmail.com", "primary": True, "verified": True}]),
                ]
            ),
            {"selector": "viewer"},
        )["members"][0]
        workspace = WORKSPACE.answer(
            fixture_api([(WORKSPACE.USERINFO_URL, {}, {"email": "alice@corp.com", "email_verified": True})]),
            {"selector": "viewer"},
        )["members"][0]

        self.assertNotEqual(github["verified_email"], workspace["verified_email"])

    def test_an_identity_without_a_verified_address_stays_qualified(self):
        slack = SLACK.answer(
            fixture_api(
                [
                    ("auth.test", {}, {"ok": True, "user_id": "U9", "team_id": "T1"}),
                    ("users.info", {"user": "U9"}, {"ok": True, "user": {"id": "U9", "profile": {}}}),
                ]
            ),
            {"selector": "viewer"},
        )["members"][0]
        github = GITHUB.answer(
            fixture_api(
                [
                    ("/user", {}, {"login": "alice", "type": "User"}),
                    ("/user/emails", {}, GITHUB.Forbidden("/user/emails")),
                ]
            ),
            {"selector": "viewer"},
        )["members"][0]

        self.assertEqual(slack, {"id": "slack:U9"})
        self.assertEqual(github, {"id": "github:alice"})


if __name__ == "__main__":
    unittest.main()
