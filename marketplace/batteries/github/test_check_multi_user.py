from http.client import HTTPMessage
from http.server import BaseHTTPRequestHandler, HTTPServer
import io
import json
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.error

sys.path.insert(0, str(Path(__file__).parent))
import check  # noqa: E402


SCRIPT = Path(__file__).with_name("check-multi-user.py")
API_ROOT = "https://github.example/api/v3"
LISTING = f"{API_ROOT}/installation/repositories?per_page=1"


class Loopback:
    """One stdlib HTTP server standing in for GitHub's REST root, answering every
    request with one status and body."""

    def __init__(self, status, answer, headers=()):
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                seen.append((self.path, self.headers.get("Authorization")))
                encoded = json.dumps(answer).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                for name, value in headers:
                    self.send_header(name, value)
                self.end_headers()
                self.wfile.write(encoded)

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

    def env(self, **extra):
        return {
            "PATH": "/usr/bin:/bin",
            "APPA_PROVIDER_GITHUB_TOKEN": "ghs-installation",
            "GITHUB_API_URL": f"http://127.0.0.1:{self.server.server_port}",
            **extra,
        }


def run_script(env):
    completed = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True, text=True, env=env)
    assert completed.returncode == 0, completed.stderr
    return json.loads(completed.stdout)


class InstallationCheck(unittest.TestCase):
    def verdict(self, status, answer, headers=()):
        def open_url(request, timeout):
            self.assertEqual(request.full_url, LISTING)
            self.assertEqual(request.get_header("Authorization"), "Bearer ghs-installation")
            self.assertEqual(timeout, 4)
            body = io.BytesIO(json.dumps(answer).encode())
            if status == 200:
                return body
            message = HTTPMessage()
            for name, value in headers:
                message[name] = value
            raise urllib.error.HTTPError(request.full_url, status, "fixture", message, body)

        environ = {"APPA_PROVIDER_GITHUB_TOKEN": "ghs-installation", "GITHUB_API_URL": API_ROOT}
        return check.check_installation(environ=environ, open_url=open_url)

    def test_the_script_checks_the_installation_over_the_wire(self):
        with Loopback(200, {"total_count": 3, "repositories": [{"full_name": "acme/widget"}]}) as github:
            result = run_script(github.env())
        self.assertEqual(github.seen, [("/installation/repositories?per_page=1", "Bearer ghs-installation")])
        self.assertEqual(result, {"status": "ready", "authentication": "token", "reason": "verified"})

    def test_an_installation_covering_a_repository_is_ready(self):
        self.assertEqual(
            self.verdict(200, {"total_count": 3, "repositories": [{"full_name": "acme/widget"}]}),
            {"status": "ready", "authentication": "token", "reason": "verified"},
        )

    def test_an_installation_covering_no_repository_needs_configuration(self):
        self.assertEqual(
            self.verdict(200, {"total_count": 0, "repositories": []}),
            {"status": "needs_configuration", "authentication": "token", "reason": "no_repositories"},
        )

    def test_a_rejected_token_is_an_invalid_credential(self):
        self.assertEqual(
            self.verdict(401, {"message": "Bad credentials"}),
            {"status": "needs_configuration", "authentication": "token", "reason": "invalid_credential"},
        )

    def test_a_personal_token_is_not_an_installation(self):
        self.assertEqual(
            self.verdict(403, {"message": "You must authenticate with an installation access token"}),
            {"status": "needs_configuration", "authentication": "token", "reason": "not_an_installation"},
        )

    def test_a_throttled_installation_is_unavailable(self):
        forbidden = {"message": "Resource not accessible by integration"}
        for headers, answer in [
            ([("x-ratelimit-remaining", "0")], forbidden),
            ([("retry-after", "60")], forbidden),
            ([], {"message": "You have exceeded a secondary rate limit. Please wait a few minutes before you try again."}),
        ]:
            with self.subTest(headers=headers, answer=answer):
                self.assertEqual(
                    self.verdict(403, answer, headers),
                    {"status": "unavailable", "authentication": "token", "reason": "provider_unavailable"},
                )

    def test_an_unset_token_is_missing_and_gh_is_never_asked(self):
        with tempfile.TemporaryDirectory() as directory:
            record = Path(directory) / "argv"
            gh = Path(directory) / "gh"
            gh.write_text(f"#!/bin/sh\necho called > {record}\nexit 0\n")
            gh.chmod(gh.stat().st_mode | stat.S_IXUSR)
            with Loopback(200, {"total_count": 1}) as github:
                result = run_script(github.env(PATH=f"{directory}:/usr/bin:/bin", APPA_PROVIDER_GITHUB_TOKEN=""))
            self.assertEqual(result, {"status": "needs_configuration", "authentication": "none", "reason": "missing_credential"})
            self.assertFalse(record.exists())
            self.assertEqual(github.seen, [])


if __name__ == "__main__":
    unittest.main()
