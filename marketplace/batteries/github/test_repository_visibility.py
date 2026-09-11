from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import threading
import unittest


SCRIPT = Path(__file__).with_name("repository-visibility.py")
SPEC = importlib.util.spec_from_file_location("repository_visibility", SCRIPT)
ANNOTATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANNOTATOR)

COLLABORATORS = "@github:repo/acme/api/collaborators"


class Loopback:
    """One stdlib HTTP server standing in for a GitHub API root."""

    def __init__(self, answers):
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                seen.append((self.path, self.headers.get("Authorization")))
                body = json.dumps(answers[self.path]).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        self.seen = seen
        self.server = HTTPServer(("127.0.0.1", 0), Handler)

    def __enter__(self):
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()

    def env(self):
        # The trailing slash is dropped by the script, not by this server.
        return {"PATH": "/usr/bin:/bin", "APPA_PROVIDER_GITHUB_TOKEN": "ghp-fixture", "GITHUB_API_URL": f"http://127.0.0.1:{self.server.server_port}/"}


def consult(name=ANNOTATOR.CONTENT, owner="acme", repo="api", **overrides):
    return {
        "version": 1,
        "kind": "annotation",
        "name": name,
        "declaration": {"ranks": ["suspicious"], "audiences": [COLLABORATORS]},
        "artifact": {"args": {"name": "get_file_contents", "arguments": {"owner": owner, "repo": repo}}},
        **overrides,
    }


class AnswerTests(unittest.TestCase):
    def test_a_read_of_a_public_repository_is_suspicious_and_public(self):
        self.assertEqual(
            ANNOTATOR.annotation(ANNOTATOR.CONTENT, "public", "acme", "api"),
            {
                "delta": {"trust": "suspicious", "audience": "public"},
                "requires": {"history": [], "attention": []},
                "emits": [],
            },
        )

    def test_a_read_of_a_private_or_internal_repository_narrows_to_its_collaborators(self):
        for visibility in ["private", "internal"]:
            answer = ANNOTATOR.annotation(ANNOTATOR.CONTENT, visibility, "acme", "api")
            self.assertEqual(answer["delta"], {"trust": "suspicious", "audience": [COLLABORATORS]})

    def test_only_a_reported_visibility_is_answered(self):
        for payload in [{"private": False}, {"visibility": "secret"}, {"visibility": None}, []]:
            with self.assertRaises(RuntimeError):
                ANNOTATOR.repository_visibility(lambda _path: payload, "acme", "api")
        self.assertEqual(
            ANNOTATOR.repository_visibility(lambda _path: {"private": False, "visibility": "internal"}, "acme", "api"),
            "internal",
        )

    def test_a_write_needs_trusted_data_everyone_the_repository_reaches_may_see(self):
        self.assertEqual(
            ANNOTATOR.annotation(ANNOTATOR.READERS, "public", "acme", "api")["requires"],
            {"trust": "trusted", "audience": {"contains": "public"}, "history": [], "attention": []},
        )
        self.assertEqual(
            ANNOTATOR.annotation(ANNOTATOR.READERS, "private", "acme", "api")["requires"]["audience"],
            {"contains": [COLLABORATORS]},
        )
        # Every enterprise member reads an internal repository, more than the collaborators.
        self.assertEqual(
            ANNOTATOR.annotation(ANNOTATOR.READERS, "internal", "acme", "api")["requires"]["audience"],
            {"contains": "public"},
        )


class ConsultTests(unittest.TestCase):
    def test_the_repository_is_read_from_the_calls_arguments(self):
        self.assertEqual(ANNOTATOR.repository_of(consult()), (ANNOTATOR.CONTENT, "acme", "api"))
        self.assertEqual(ANNOTATOR.repository_of(consult(name=ANNOTATOR.READERS))[0], ANNOTATOR.READERS)

    def test_a_foreign_consult_is_refused(self):
        for request in [
            consult(version=2),
            consult(kind="audience"),
            consult(name="github.other"),
            consult(artifact={}),
            consult(artifact={"args": {"name": "get_me", "arguments": {}}}),
        ]:
            with self.assertRaises(ValueError):
                ANNOTATOR.repository_of(request)

    def test_a_repository_segment_the_placeholder_would_refuse_is_refused_here(self):
        for owner, repo in [("", "api"), ("acme", ""), ("acme/x", "api"), ("acme", "a/b"), ("$owner", "api"), (7, "api")]:
            with self.assertRaises(ValueError):
                ANNOTATOR.repository_of(consult(owner=owner, repo=repo))


class EnvelopeTests(unittest.TestCase):
    def run_script(self, request, env):
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=json.dumps(request),
            capture_output=True,
            text=True,
            env=env,
        )

    def test_the_api_root_is_github_api_url(self):
        with Loopback({"/repos/acme/api": {"visibility": "private"}}) as github:
            result = self.run_script(consult(), github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(github.seen, [("/repos/acme/api", "Bearer ghp-fixture")])
        self.assertEqual(json.loads(result.stdout)["answer"]["delta"], {"trust": "suspicious", "audience": [COLLABORATORS]})

    def test_a_missing_token_is_a_failure_before_any_network(self):
        result = self.run_script(consult(), {"PATH": "/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("APPA_PROVIDER_GITHUB_TOKEN", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_mandate_naming_another_collection_is_refused_before_the_token_is_read(self):
        for audiences in [["@github:repo/acme/other/collaborators"], ["github:internal"], []]:
            request = consult(declaration={"ranks": ["suspicious"], "audiences": audiences})
            result = self.run_script(request, {"PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
