import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import check  # noqa: E402
import service_principal  # noqa: E402

SCRIPT = Path(__file__).with_name("audience-source-multi-user.py")
CHECK = Path(__file__).with_name("check-multi-user.py")
TEMPLATES = ["members", "group/<name>", "genie-space/<id>/readers"]
LOGIN = {
    "APPA_PROVIDER_DATABRICKS_HOST": "https://workspace.example",
    "APPA_PROVIDER_DATABRICKS_CLIENT_ID": "principal-fixture",
    "APPA_PROVIDER_DATABRICKS_CLIENT_SECRET": "secret-fixture",
}
CLI_LOGIN = {
    "DATABRICKS_HOST": "https://workspace.example",
    "DATABRICKS_CLIENT_ID": "principal-fixture",
    "DATABRICKS_CLIENT_SECRET": "secret-fixture",
}
PERSONAL = {"DATABRICKS_TOKEN": "dapi-personal-fixture", "DATABRICKS_CONFIG_PROFILE": "personal"}
SEEN = ["DATABRICKS_AUTH_TYPE", "DATABRICKS_CONFIG_FILE", "DATABRICKS_TOKEN", "DATABRICKS_CONFIG_PROFILE", "DATABRICKS_CLIENT_ID"]


class FakeDatabricks:
    """A `databricks` executable on its own PATH entry: it records each
    invocation's arguments and the login variables it saw, and answers with
    the script's stdout, stderr, and exit status."""

    def __init__(self, script):
        self.directory = tempfile.TemporaryDirectory()
        self.log = Path(self.directory.name) / "calls.jsonl"
        cli = Path(self.directory.name) / "databricks"
        cli.write_text(
            "#!/bin/sh\n"
            f"python3 -c 'import json,os,sys; print(json.dumps({{\"args\": sys.argv[1:], **{{name: os.environ.get(name) for name in {json.dumps(SEEN)}}}}}))' \"$@\" >> {self.log}\n"
            f"{script}\n"
        )
        cli.chmod(cli.stat().st_mode | stat.S_IEXEC)

    def environ(self, **extra):
        return {"PATH": f"{self.directory.name}:/usr/bin:/bin", **LOGIN, **PERSONAL, **extra}

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]


def as_principal(*args):
    return {
        "args": [*args, "-o", "json"],
        "DATABRICKS_AUTH_TYPE": "oauth-m2m",
        "DATABRICKS_CONFIG_FILE": os.devnull,
        "DATABRICKS_TOKEN": None,
        "DATABRICKS_CONFIG_PROFILE": None,
        "DATABRICKS_CLIENT_ID": "principal-fixture",
    }


class EnvironmentTests(unittest.TestCase):
    def test_only_the_principals_login_reaches_the_cli(self):
        environ = {
            "PATH": "/bin",
            **LOGIN,
            **PERSONAL,
            "DATABRICKS_CONFIG_FILE": "/home/ana/.databrickscfg",
            "DATABRICKS_HOST": "https://personal.example",
        }
        self.assertEqual(
            service_principal.environment(environ),
            {"PATH": "/bin", **CLI_LOGIN, "DATABRICKS_AUTH_TYPE": "oauth-m2m", "DATABRICKS_CONFIG_FILE": os.devnull},
        )

    def test_the_cli_login_variables_alone_are_no_login(self):
        with self.assertRaises(service_principal.MissingLogin):
            service_principal.environment({"PATH": "/bin", **CLI_LOGIN})

    def test_a_missing_or_blank_login_variable_is_refused(self):
        for name in LOGIN:
            for environ in [{**LOGIN, name: " "}, {key: value for key, value in LOGIN.items() if key != name}]:
                with self.assertRaises(service_principal.MissingLogin):
                    service_principal.environment(environ)


class SourceTests(unittest.TestCase):
    def run_script(self, artifact, env, templates=TEMPLATES):
        request = {
            "version": 1,
            "kind": "audience",
            "name": "databricks",
            "declaration": {"templates": templates},
            "artifact": artifact,
        }
        return subprocess.run(
            [sys.executable, str(SCRIPT)], input=json.dumps(request), capture_output=True, text=True, env=env, check=False
        )

    def test_members_are_read_as_the_service_principal(self):
        cli = FakeDatabricks("echo '[{\"id\": \"1\", \"userName\": \"ana@corp.example\", \"active\": true}]'")
        result = self.run_script({"selector": "members"}, cli.environ())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"version": 1, "answer": {"members": ["ana@corp.example"]}})
        self.assertEqual(
            cli.calls(), [as_principal("users", "list", "--attributes", "id,userName,active", "--filter", "active eq true")]
        )

    def test_a_member_resolves_to_its_address_as_the_service_principal(self):
        cli = FakeDatabricks("echo '{\"id\": \"7\", \"userName\": \"ana@corp.example\", \"active\": true}'")
        result = self.run_script({"member": "databricks:7"}, cli.environ())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"version": 1, "answer": {"principal": "ana@corp.example"}})
        self.assertEqual(cli.calls(), [as_principal("users", "get", "7")])

    def test_the_viewer_is_refused_before_the_cli_runs(self):
        cli = FakeDatabricks("echo '{}'")
        for templates, status in [(TEMPLATES, 1), (["viewer", *TEMPLATES], 2)]:
            result = self.run_script({"selector": "viewer"}, cli.environ(), templates)
            self.assertEqual(result.returncode, status, result.stderr)
            self.assertEqual(result.stdout, "")
        self.assertEqual(cli.calls(), [])

    def test_without_the_principals_login_the_cli_never_runs(self):
        cli = FakeDatabricks("echo '[]'")
        environ = {key: value for key, value in cli.environ().items() if key != "APPA_PROVIDER_DATABRICKS_CLIENT_SECRET"}
        result = self.run_script({"selector": "members"}, environ)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(cli.calls(), [])


class CheckTests(unittest.TestCase):
    def test_the_principals_login_is_verified(self):
        cli = FakeDatabricks("echo '{\"id\": \"9\", \"userName\": \"principal-fixture\", \"active\": true}'")
        self.assertEqual(
            check.check_service_principal(cli.environ()),
            {"status": "ready", "authentication": "token", "reason": "verified"},
        )
        self.assertEqual(cli.calls(), [as_principal("current-user", "me")])

    def test_a_missing_login_needs_configuration_without_the_cli(self):
        cli = FakeDatabricks("echo '{\"id\": \"1\"}'")
        environ = {key: value for key, value in cli.environ().items() if key != "APPA_PROVIDER_DATABRICKS_HOST"}
        self.assertEqual(
            check.check_service_principal(environ),
            {"status": "needs_configuration", "authentication": "token", "reason": "missing_configuration"},
        )
        self.assertEqual(cli.calls(), [])

    def test_a_refused_login_is_unavailable(self):
        cli = FakeDatabricks("echo 'Error: oauth-m2m: invalid_client' >&2; exit 1")
        self.assertEqual(check.check_service_principal(cli.environ())["status"], "unavailable")

    def test_the_script_prints_the_verdict(self):
        cli = FakeDatabricks("echo '{\"id\": \"9\"}'")
        result = subprocess.run(
            [sys.executable, str(CHECK)], capture_output=True, text=True, env=cli.environ(), cwd=CHECK.parent, check=False
        )
        self.assertEqual(json.loads(result.stdout), {"status": "ready", "authentication": "token", "reason": "verified"})


if __name__ == "__main__":
    unittest.main()
