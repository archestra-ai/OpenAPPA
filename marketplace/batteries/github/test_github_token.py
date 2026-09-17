import importlib.util
import os
from pathlib import Path
import stat
import tempfile
import unittest


MODULE = Path(__file__).with_name("github_token.py")
SPEC = importlib.util.spec_from_file_location("github_token", MODULE)
GITHUB_TOKEN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GITHUB_TOKEN)


class FakeGh:
    """A `gh` on its own PATH entry that records its arguments and answers as told."""

    def __init__(self, script):
        self.directory = tempfile.TemporaryDirectory()
        self.record = Path(self.directory.name) / "argv"
        gh = Path(self.directory.name) / "gh"
        gh.write_text(f"#!/bin/sh\nprintf '%s\\n' \"$@\" > {self.record}\n{script}\n")
        gh.chmod(gh.stat().st_mode | stat.S_IXUSR)

    def environ(self, **extra):
        return {"PATH": self.directory.name, **extra}

    def argv(self):
        return self.record.read_text().split()

    def close(self):
        self.directory.cleanup()


class ResolveToken(unittest.TestCase):
    def test_the_variable_wins_without_asking_gh(self):
        gh = FakeGh("echo from-gh")
        self.addCleanup(gh.close)

        token = GITHUB_TOKEN.resolve_token(gh.environ(APPA_PROVIDER_GITHUB_TOKEN="ghp-fixture"))

        self.assertEqual(token, "ghp-fixture")
        self.assertFalse(gh.record.exists())

    def test_a_padded_variable_is_stripped_and_a_blank_one_is_unset(self):
        gh = FakeGh("echo gho-from-gh")
        self.addCleanup(gh.close)

        padded = GITHUB_TOKEN.resolve_token(gh.environ(APPA_PROVIDER_GITHUB_TOKEN=" ghp-fixture\r\n"))
        blank = GITHUB_TOKEN.resolve_token(gh.environ(APPA_PROVIDER_GITHUB_TOKEN="  "))

        self.assertEqual(padded, "ghp-fixture")
        self.assertEqual(blank, "gho-from-gh")

    def test_gh_login_answers_when_the_variable_is_unset(self):
        gh = FakeGh("echo gho-from-gh")
        self.addCleanup(gh.close)

        token = GITHUB_TOKEN.resolve_token(gh.environ())

        self.assertEqual(token, "gho-from-gh")
        self.assertEqual(gh.argv(), ["auth", "token", "--hostname", "github.com"])

    def test_an_enterprise_api_root_asks_gh_for_that_host(self):
        gh = FakeGh("echo ghe-token")
        self.addCleanup(gh.close)

        token = GITHUB_TOKEN.resolve_token(gh.environ(GITHUB_API_URL="https://ghe.example/api/v3"))

        self.assertEqual(token, "ghe-token")
        self.assertEqual(gh.argv(), ["auth", "token", "--hostname", "ghe.example"])

    def test_a_gh_that_is_not_logged_in_names_both_fixes(self):
        gh = FakeGh("echo 'not logged in' >&2; exit 1")
        self.addCleanup(gh.close)

        with self.assertRaises(RuntimeError) as refused:
            GITHUB_TOKEN.resolve_token(gh.environ())

        self.assertIn("APPA_PROVIDER_GITHUB_TOKEN", str(refused.exception))
        self.assertIn("gh auth login", str(refused.exception))

    def test_no_gh_on_path_names_both_fixes(self):
        empty = tempfile.TemporaryDirectory()
        self.addCleanup(empty.cleanup)

        with self.assertRaises(RuntimeError) as refused:
            GITHUB_TOKEN.resolve_token({"PATH": empty.name})

        self.assertIn("APPA_PROVIDER_GITHUB_TOKEN", str(refused.exception))
        self.assertIn("gh auth login", str(refused.exception))

    def test_an_empty_gh_answer_is_no_token(self):
        gh = FakeGh("echo ''")
        self.addCleanup(gh.close)

        with self.assertRaises(RuntimeError):
            GITHUB_TOKEN.resolve_token(gh.environ())


if __name__ == "__main__":
    unittest.main()
