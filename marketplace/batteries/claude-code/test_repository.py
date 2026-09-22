import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("repository.py")
SPEC = importlib.util.spec_from_file_location("repository", SCRIPT)
INPUT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INPUT)


def consult(command, cwd=None):
    artifact = {"tool": "host/claude-code/Bash", "arguments": {"command": command}}
    if cwd is not None:
        artifact["cwd"] = cwd
    return {"version": 1, "kind": "input", "name": "claude-code.repository", "declaration": {}, "artifact": artifact}


class RepositoryNamed(unittest.TestCase):
    def test_a_gh_call_names_its_repository_with_the_repo_flag(self):
        for command in (
            "gh pr create --repo acme/api --title x",
            "gh issue list -R acme/api",
            "gh api --repo=acme/api repos/{owner}/{repo}",
        ):
            self.assertEqual(INPUT.repository_named(command), {"slug": "acme/api"}, command)

    def test_a_push_names_a_url_or_a_remote(self):
        self.assertEqual(
            INPUT.repository_named("git push https://github.com/acme/api.git main"),
            {"slug": "https://github.com/acme/api.git"},
        )
        self.assertEqual(INPUT.repository_named("git push git@github.com:acme/api.git"), {"slug": "git@github.com:acme/api.git"})
        self.assertEqual(INPUT.repository_named("cd /w && git push -u upstream feat 2>&1 | tail -2"), {"remote": "upstream"})
        self.assertEqual(INPUT.repository_named("git -C /w push --force-with-lease origin main"), {"remote": "origin"})

    def test_a_bare_push_or_gh_call_names_nothing(self):
        for command in ("git push", "git push --tags", "gh pr create --draft", "gh repo view"):
            self.assertIsNone(INPUT.repository_named(command), command)


class RepositoryOf(unittest.TestCase):
    def test_a_call_without_a_command_or_a_directory_is_an_unestablished_finding(self):
        finding = INPUT.repository_of(consult("gh pr create --draft"))
        self.assertEqual((finding["name_with_owner"], finding["visibility"]), (None, None))
        self.assertIn("directory", finding["reason"])
        finding = INPUT.repository_of({"version": 1, "kind": "input", "artifact": {"tool": "Read", "arguments": {"file_path": "x"}}})
        self.assertEqual(finding["visibility"], None)

    def test_a_directory_that_is_no_checkout_is_an_unestablished_finding(self):
        with tempfile.TemporaryDirectory() as empty:
            finding = INPUT.repository_of(consult("git push origin main", cwd=empty))
        self.assertEqual(finding["visibility"], None)
        self.assertTrue(finding["reason"])

    def test_a_consult_of_another_kind_is_refused(self):
        with self.assertRaises(ValueError):
            INPUT.repository_of({"version": 1, "kind": "annotation", "artifact": {"args": {}}})


class Program(unittest.TestCase):
    def test_the_program_answers_the_envelope_with_a_zero_exit(self):
        completed = subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=json.dumps(consult("gh pr create --draft")),
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        answer = json.loads(completed.stdout)
        self.assertEqual(answer["version"], 1)
        self.assertEqual(answer["answer"]["visibility"], None)

    def test_an_oversized_or_foreign_consult_exits_nonzero(self):
        for body in ("x" * (INPUT.MAX_INPUT_BYTES + 1), json.dumps({"version": 2})):
            completed = subprocess.run([sys.executable, str(SCRIPT)], input=body, capture_output=True, text=True, check=False)
            self.assertNotEqual(completed.returncode, 0)


if __name__ == "__main__":
    unittest.main()
