import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("audience-source.py")
SPEC = importlib.util.spec_from_file_location("monday_audience", SCRIPT)
SOURCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SOURCE)


def user(user_id="123", **changes):
    value = {"id": user_id, "email": "alice@corp.example", "is_email_confirmed": True, "status": "ACTIVE"}
    value.update(changes)
    return value


class RecipientTests(unittest.TestCase):
    def test_confirmed_active_user_is_the_only_reader(self):
        self.assertEqual(
            SOURCE.answer(lambda user_id: [user(user_id)], {"selector": "user/123"}),
            {"members": ["alice@corp.example"]},
        )

    def test_invalid_selectors_are_refused_before_provider_lookup(self):
        def unexpected(_user_id):
            self.fail("invalid selector reached the provider")

        for artifact in [
            {"selector": "user/"},
            {"selector": "user/123/other"},
            {"selector": "user/-1"},
            {"selector": "user/1) { users { email } }"},
            {"selector": "team/123"},
            {"member": "monday:123"},
            {"selector": "user/123", "extra": True},
        ]:
            with self.subTest(artifact=artifact), self.assertRaises(ValueError):
                SOURCE.answer(unexpected, artifact)

    def test_missing_or_unverified_recipient_is_refused(self):
        for users in [
            [],
            [user("456")],
            [user(status="INACTIVE")],
            [user(is_email_confirmed=False)],
            [user(email="")],
            [user(email="alice@corp.example"), user(email="bob@corp.example")],
            None,
        ]:
            with self.subTest(users=users), self.assertRaises(RuntimeError):
                SOURCE.answer(lambda _user_id: users, {"selector": "user/123"})

    def test_lookup_sends_exact_id_to_versioned_users_api(self):
        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return json.dumps({"data": {"users": [user()]}}).encode()

        with mock.patch.object(SOURCE.urllib.request, "urlopen", return_value=Response()) as opened:
            self.assertEqual(SOURCE.graphql("test-token")("123"), [user()])
        request = opened.call_args.args[0]
        self.assertEqual(request.full_url, SOURCE.API_URL)
        self.assertEqual(request.get_header("Authorization"), "test-token")
        self.assertEqual(request.get_header("Api-version"), SOURCE.API_VERSION)
        self.assertEqual(json.loads(request.data)["variables"], {"ids": ["123"]})

    def test_invalid_declaration_is_refused_before_token_lookup(self):
        request = {
            "version": 1,
            "kind": "audience",
            "name": "monday",
            "declaration": {"templates": ["user/<id>", "full-members"]},
            "artifact": {"selector": "user/123"},
        }
        result = subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=json.dumps(request),
            text=True,
            capture_output=True,
            env={},
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("policy declares", result.stderr)
        self.assertNotIn("APPA_PROVIDER_MONDAY_TOKEN is not set", result.stderr)


if __name__ == "__main__":
    unittest.main()
