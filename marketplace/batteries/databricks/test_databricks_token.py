import importlib.util
from pathlib import Path
import stat
import tempfile
import unittest


MODULE = Path(__file__).with_name("databricks_token.py")
SPEC = importlib.util.spec_from_file_location("databricks_token", MODULE)
DATABRICKS_TOKEN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DATABRICKS_TOKEN)

DESCRIBE = '{"details": {"host": "https://cli.cloud.databricks.com", "username": "cli@corp.com"}}'
TOKEN = '{"access_token": "cli-token", "token_type": "Bearer"}'


class FakeCli:
    """A `databricks` on its own PATH entry that records each invocation and
    answers `auth describe` and `auth token` as told."""

    def __init__(self, describe=DESCRIBE, token=TOKEN):
        self.directory = tempfile.TemporaryDirectory()
        self.record = Path(self.directory.name) / "argv"
        cli = Path(self.directory.name) / "databricks"
        cli.write_text(
            "#!/bin/sh\n"
            f'printf \'%s\\n\' "$@" >> {self.record}\n'
            f"case \"$2\" in describe) {describe};; token) {token};; *) exit 3;; esac\n"
        )
        cli.chmod(cli.stat().st_mode | stat.S_IXUSR)

    def environ(self, **extra):
        return {"PATH": self.directory.name, **extra}

    def argv(self):
        return self.record.read_text().split() if self.record.exists() else []

    def close(self):
        self.directory.cleanup()


def answering(text):
    return f"printf '%s' '{text}'"


class ResolveTests(unittest.TestCase):
    def cli(self, **answers):
        cli = FakeCli(**{name: answering(text) for name, text in answers.items()})
        self.addCleanup(cli.close)
        return cli

    def test_the_variables_win_without_running_the_cli(self):
        cli = self.cli(describe=DESCRIBE, token=TOKEN)

        resolved = DATABRICKS_TOKEN.resolve(
            cli.environ(
                APPA_PROVIDER_DATABRICKS_HOST="https://var.cloud.databricks.com/",
                APPA_PROVIDER_DATABRICKS_TOKEN=" dapi-fixture\n",
            )
        )

        self.assertEqual(resolved, ("https://var.cloud.databricks.com", "dapi-fixture"))
        self.assertEqual(cli.argv(), [])

    def test_the_sdk_host_variable_is_read_after_the_battery_one(self):
        cli = self.cli(describe=DESCRIBE, token=TOKEN)

        resolved = DATABRICKS_TOKEN.resolve(
            cli.environ(DATABRICKS_HOST="sdk.cloud.databricks.com", APPA_PROVIDER_DATABRICKS_TOKEN="dapi-fixture")
        )

        self.assertEqual(resolved, ("https://sdk.cloud.databricks.com", "dapi-fixture"))
        self.assertEqual(cli.argv(), [])

    def test_the_cli_login_answers_host_and_token_when_the_variables_are_unset(self):
        cli = self.cli(describe=DESCRIBE, token=TOKEN)

        resolved = DATABRICKS_TOKEN.resolve(cli.environ())

        self.assertEqual(resolved, ("https://cli.cloud.databricks.com", "cli-token"))
        self.assertEqual(
            cli.argv(),
            ["auth", "describe", "-o", "json", "auth", "token", "--host", "https://cli.cloud.databricks.com"],
        )

    def test_a_blank_token_variable_is_unset_and_the_cli_answers_for_the_given_host(self):
        cli = self.cli(describe=DESCRIBE, token=TOKEN)

        resolved = DATABRICKS_TOKEN.resolve(
            cli.environ(APPA_PROVIDER_DATABRICKS_HOST="var.cloud.databricks.com", APPA_PROVIDER_DATABRICKS_TOKEN="  ")
        )

        self.assertEqual(resolved, ("https://var.cloud.databricks.com", "cli-token"))
        self.assertEqual(cli.argv(), ["auth", "token", "--host", "https://var.cloud.databricks.com"])

    def test_a_cli_that_is_not_logged_in_names_both_fixes(self):
        cli = self.cli(describe=DESCRIBE, token="")
        cli_without_login = FakeCli(describe=answering(DESCRIBE), token="echo 'not logged in' >&2; exit 1")
        self.addCleanup(cli_without_login.close)

        for fake in (cli, cli_without_login):
            with self.assertRaises(RuntimeError) as refused:
                DATABRICKS_TOKEN.resolve(fake.environ())
            self.assertIn("APPA_PROVIDER_DATABRICKS_TOKEN", str(refused.exception))
            self.assertIn("databricks auth login --host https://cli.cloud.databricks.com", str(refused.exception))

    def test_no_cli_on_path_and_no_host_names_both_fixes(self):
        empty = tempfile.TemporaryDirectory()
        self.addCleanup(empty.cleanup)

        with self.assertRaises(RuntimeError) as refused:
            DATABRICKS_TOKEN.resolve({"PATH": empty.name})

        self.assertIn("APPA_PROVIDER_DATABRICKS_HOST", str(refused.exception))
        self.assertIn("databricks auth login", str(refused.exception))

    def test_a_cli_answer_that_is_not_json_is_no_host(self):
        cli = self.cli(describe="Host: https://cli.cloud.databricks.com", token=TOKEN)

        with self.assertRaises(RuntimeError) as refused:
            DATABRICKS_TOKEN.resolve(cli.environ(APPA_PROVIDER_DATABRICKS_TOKEN="dapi-fixture"))

        self.assertIn("APPA_PROVIDER_DATABRICKS_HOST", str(refused.exception))

    def test_the_sdk_token_variable_is_never_read(self):
        cli = self.cli(describe=DESCRIBE, token="")

        with self.assertRaises(RuntimeError):
            DATABRICKS_TOKEN.resolve(cli.environ(DATABRICKS_TOKEN="dapi-sdk"))


class WorkspaceUrlTests(unittest.TestCase):
    def test_a_host_is_normalised_to_its_https_origin(self):
        for spelling in ["dbc-1.cloud.databricks.com", "https://dbc-1.cloud.databricks.com/", " https://dbc-1.cloud.databricks.com \n"]:
            self.assertEqual(DATABRICKS_TOKEN.workspace_url(spelling), "https://dbc-1.cloud.databricks.com")

    def test_anything_but_a_bare_https_host_is_refused(self):
        for spelling in ["", "   ", None, "http://dbc-1.cloud.databricks.com", "https://dbc-1.cloud.databricks.com/api", "https://"]:
            self.assertIsNone(DATABRICKS_TOKEN.workspace_url(spelling))


if __name__ == "__main__":
    unittest.main()
