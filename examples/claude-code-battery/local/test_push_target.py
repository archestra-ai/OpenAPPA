import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location("push_target", Path(__file__).with_name("push-target.py"))
assert SPEC is not None and SPEC.loader is not None
push_target = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(push_target)

SINK = "https://github.com/archestra-ai/openappa-sink.git"
OTHER = "https://github.com/archestra-ai/OpenAPPA.git"


def repo(remote_url, push_urls=()):
    """A fresh repository on `main` whose `origin` fetches `remote_url` and pushes to
    `push_urls` when given."""
    directory = tempfile.mkdtemp()
    subprocess.run(["git", "-c", "init.defaultBranch=main", "init", "-q", directory], check=True)
    subprocess.run(["git", "-C", directory, "remote", "add", "origin", remote_url], check=True)
    for url in push_urls:
        subprocess.run(["git", "-C", directory, "remote", "set-url", "--add", "--push", "origin", url], check=True)
    return directory


def consult(command, cwd):
    return {
        "version": 1,
        "kind": "annotation",
        "name": "local.push-target",
        "declaration": {
            "inputs": [],
            "trust_ranks": ["suspicious", "trusted"],
            "audiences": [],
            "attention_marks": ["hitl"],
            "effects": [],
        },
        "artifact": {"args": {"name": "Bash", "arguments": {"command": command}}, "cwd": cwd},
    }


class GrammarTests(unittest.TestCase):
    def test_a_plain_push_names_its_remote(self):
        self.assertEqual(push_target.parse_push("git push origin main"), {"remote": "origin"})
        self.assertEqual(push_target.parse_push("git push -u origin HEAD:main"), {"remote": "origin"})
        self.assertEqual(push_target.parse_push("git push --force-with-lease=main origin"), {"remote": "origin"})
        self.assertEqual(push_target.parse_push("git push"), {"remote": None})

    def test_every_other_shape_is_not_a_plain_push(self):
        for command in [
            "GIT_DIR=/elsewhere/.git git push origin main",
            "git -C /elsewhere push origin main",
            "git --git-dir=/elsewhere/.git push origin main",
            "git push --repo=https://github.com/x/y origin main",
            "git push https://github.com/archestra-ai/openappa-sink.git main",
            "git push git@github.com:archestra-ai/openappa-sink.git main",
            "cd /elsewhere && git push origin main",
            "git push origin main; rm -rf /",
            "git push $(cat remote) main",
            "git push -- origin main",
            "git status",
        ]:
            self.assertIsNone(push_target.parse_push(command), command)


class DestinationTests(unittest.TestCase):
    def test_the_allowed_repository_needs_nothing(self):
        self.assertFalse(push_target.decide("git push origin main", repo(SINK)))
        self.assertFalse(push_target.decide("git push origin main", repo("git@github.com:archestra-ai/openappa-sink.git")))
        self.assertFalse(push_target.decide("git push origin main", repo("ssh://git@github.com/Archestra-AI/OpenAPPA-Sink")))

    def test_another_repository_needs_attention(self):
        self.assertTrue(push_target.decide("git push origin main", repo(OTHER)))

    def test_the_push_url_wins_over_the_fetch_url(self):
        self.assertTrue(push_target.decide("git push origin main", repo(SINK, push_urls=[OTHER])))
        self.assertFalse(push_target.decide("git push origin main", repo(OTHER, push_urls=[SINK])))

    def test_every_push_url_must_be_allowed(self):
        self.assertTrue(push_target.decide("git push origin main", repo(SINK, push_urls=[SINK, OTHER])))

    def test_a_bare_push_resolves_the_remote_as_git_does(self):
        directory = repo(SINK)
        subprocess.run(["git", "-C", directory, "remote", "add", "mirror", OTHER], check=True)
        subprocess.run(["git", "-C", directory, "config", "branch.main.remote", "mirror"], check=True)
        self.assertTrue(push_target.decide("git push", directory))
        subprocess.run(["git", "-C", directory, "config", "branch.main.pushRemote", "origin"], check=True)
        self.assertFalse(push_target.decide("git push", directory))

    def test_an_unknown_remote_or_directory_needs_attention(self):
        self.assertTrue(push_target.decide("git push upstream main", repo(SINK)))
        self.assertTrue(push_target.decide("git push origin main", tempfile.mkdtemp()))


class ConsultTests(unittest.TestCase):
    def run_script(self, request):
        return subprocess.run(
            [sys.executable, str(Path(__file__).with_name("push-target.py"))],
            input=json.dumps(request),
            capture_output=True,
            text=True,
            check=False,
        )

    def test_the_answer_is_the_complete_annotation(self):
        result = self.run_script(consult("git push origin main", repo(OTHER)))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout),
            {"version": 1, "answer": {"delta": {}, "requires": {"history": [], "attention": ["hitl"]}, "emits": []}},
        )
        result = self.run_script(consult("git push origin main", repo(SINK)))
        self.assertEqual(
            json.loads(result.stdout)["answer"]["requires"]["attention"],
            [],
        )

    def test_a_missing_working_directory_is_an_error_not_an_answer(self):
        result = self.run_script(consult("git push origin main", None))
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("artifact.cwd is required", result.stderr)


if __name__ == "__main__":
    unittest.main()
