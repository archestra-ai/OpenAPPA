"""Provider checks use fixture responses only: no credentials or network required."""
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import unittest
import urllib.error
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1] / "marketplace" / "batteries"


def load(name):
    directory = ROOT / name
    sys.path.insert(0, str(directory))
    try:
        spec = importlib.util.spec_from_file_location(name + "_readiness", directory / "check.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        sys.path.pop(0)


GITHUB = load("github")
SLACK = load("slack")
HF = load("huggingface")
DATABRICKS = load("databricks")


class ReadinessTests(unittest.TestCase):
    def assert_safe(self, result):
        self.assertEqual(set(result), {"status", "authentication", "reason"})
        self.assertNotIn("fixture-secret", json.dumps(result))

    def test_github_explicit_token_checks_provider_access(self):
        calls = []
        def request(req, timeout):
            self.assertEqual(req.get_header("Authorization"), "Bearer fixture-secret")
            calls.append(req.full_url)
            return io.BytesIO(json.dumps([] if req.full_url.endswith("/emails") else {"login": "fixture"}).encode())
        result = GITHUB.check({GITHUB.TOKEN_VAR: "fixture-secret"}, request)
        self.assertEqual(result["status"], "ready")
        self.assertEqual(len(calls), 2)
        self.assert_safe(result)
    def test_github_cli_uses_active_account_status_without_extracting_tokens(self):
        env = {"GITHUB_API_URL": "https://github.example.test/api/v3"}
        def run(argv, **kwargs):
            self.assertEqual(argv, ["gh", "auth", "status", "--active", "--hostname", "github.example.test"])
            self.assertEqual(kwargs["stdout"], subprocess.DEVNULL)
            self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
            self.assertEqual(kwargs["env"], env)
            self.assertLess(kwargs["timeout"], 15)
            return subprocess.CompletedProcess(argv, 0)
        def no_api(*args, **kwargs):
            self.fail("CLI readiness must use gh auth status, not direct HTTP")
        result = GITHUB.check(env, no_api, run)
        self.assertEqual(result["status"], "ready")
        self.assertEqual(result["authentication"], "cli")
        self.assert_safe(result)
        result = GITHUB.check({}, no_api, lambda *a, **kw: subprocess.CompletedProcess(a, 1))
        self.assertEqual(result["reason"], "cli_not_authenticated")
        for error, reason in [(FileNotFoundError(), "missing_executable"),
                              (subprocess.TimeoutExpired("gh", 8), "check_timed_out"),
                              (OSError(), "check_failed")]:
            with patch.object(GITHUB.subprocess, "run", side_effect=error) as failed:
                result = GITHUB.check({}, no_api, failed)
            self.assertEqual(result["reason"], reason)
            self.assert_safe(result)

    def test_github_auth_scope_and_network_failures_are_distinct(self):
        for code, reason in [(401, "invalid_credential"), (403, "insufficient_access"), (503, "provider_unavailable")]:
            def request(req, timeout):
                raise urllib.error.HTTPError(req.full_url, code, "fixture-secret", {}, None)
            result = GITHUB.check({GITHUB.TOKEN_VAR: "fixture-secret"}, request)
            self.assertEqual(result["reason"], reason)
            self.assert_safe(result)

    def test_slack_missing_token_and_provider_errors(self):
        self.assertEqual(SLACK.check({})["reason"], "missing_credential")
        for response, expected in [({"ok": True}, "verified"), ({"ok": False, "error": "invalid_auth"}, "invalid_credential"),
                                   ({"ok": False, "error": "missing_scope"}, "insufficient_access")]:
            def api(token):
                self.assertEqual(token, "fixture-secret")
                def call(method):
                    self.assertEqual(method, "auth.test")
                    return response
                return call
            result = SLACK.check({SLACK.TOKEN_VAR: "fixture-secret"}, api)
            self.assertEqual(result["reason"], expected)
            self.assert_safe(result)

    def test_huggingface_validates_cached_cli_login(self):
        with patch.object(HF, "resolve_token", return_value="fixture-secret"):
            def request(req, timeout):
                self.assertTrue(req.full_url.endswith("/api/whoami-v2"))
                return io.BytesIO(b'{"name":"fixture"}')
            result = HF.check({}, request)
        self.assertEqual(result["status"], "ready")
        self.assertEqual(result["authentication"], "cli")
        self.assert_safe(result)

    def test_databricks_maps_token_and_keeps_cli_failures_unclassified(self):
        def run(argv, **kwargs):
            self.assertEqual(argv, ["databricks", "current-user", "me", "-o", "json"])
            self.assertEqual(kwargs["env"]["DATABRICKS_TOKEN"], "fixture-secret")
            return subprocess.CompletedProcess(argv, 0, '{"id":"fixture"}', '')
        env = {DATABRICKS.TOKEN_VAR: "fixture-secret", "DATABRICKS_HOST": "https://workspace.example"}
        result = DATABRICKS.check(env, run)
        self.assertEqual(result["status"], "ready")
        self.assert_safe(result)
        self.assertEqual(DATABRICKS.check({DATABRICKS.TOKEN_VAR: "fixture-secret"}, run)["reason"], "missing_configuration")
        result = DATABRICKS.check({}, lambda *args, **kwargs: subprocess.CompletedProcess(args, 1, '', 'fixture-secret'))
        self.assertEqual(result["status"], "unavailable")
        self.assert_safe(result)


if __name__ == "__main__":
    unittest.main()
