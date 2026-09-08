"""Cross-battery acceptance: the member id an audience source reports is
the reader. Two providers that attest the same address report the same
reader, so their audiences merge on it; a member without an attested
address is its provider-qualified id, which merges with nothing. These
tests pin the members the real scripts emit from recorded provider
payloads, with no network.
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


class CrossBatteryReaderTests(unittest.TestCase):
    def test_workspace_and_slack_attesting_the_same_address_report_the_same_reader(self):
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
                        {
                            "ok": True,
                            "user": {
                                "id": "U012345",
                                "profile": {"email": "alice@corp.com"},
                                "is_email_confirmed": True,
                            },
                        },
                    ),
                ]
            ),
            {"selector": "viewer"},
        )["members"][0]

        self.assertEqual(workspace, "alice@corp.com")
        self.assertEqual(slack, workspace)

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

        self.assertNotEqual(github, workspace)

    def test_a_member_without_an_attested_address_is_its_qualified_id(self):
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

        self.assertEqual(slack, "slack:U9")
        self.assertEqual(github, "github:alice")


if __name__ == "__main__":
    unittest.main()
