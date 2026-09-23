from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import unittest
import urllib.parse


SCRIPT = Path(__file__).with_name("audience-source.py")
TOKEN = "archestra-key"
TEMPLATES = ["members", "team/<team>", "user/<user>"]


class RecordedArchestra:
    """A local Archestra audience API answering from recorded payloads."""

    def __init__(self, answers):
        self.answers = answers
        self.seen = []
        recorded = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                url = urllib.parse.urlsplit(self.path)
                query = tuple(sorted(urllib.parse.parse_qsl(url.query)))
                recorded.seen.append((url.path, query, self.headers.get("Authorization")))
                status, body = recorded.answers.get(query, (404, {"error": "not recorded"}))
                if self.headers.get("Authorization") != f"Bearer {TOKEN}":
                    status, body = 401, {"error": "unauthenticated"}
                payload = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *_):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)

    def __enter__(self):
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()

    @property
    def base_url(self):
        host, port = self.server.server_address
        return f"http://{host}:{port}/"


def consult(artifact, base_url, token=TOKEN, templates=TEMPLATES):
    request = {
        "version": 1,
        "kind": "audience",
        "name": "archestra",
        "declaration": {"templates": templates},
        "artifact": artifact,
    }
    env = {key: value for key, value in os.environ.items() if key not in ("ARCHESTRA_BASE_URL", "APPA_PROVIDER_ARCHESTRA_TOKEN")}
    if base_url is not None:
        env["ARCHESTRA_BASE_URL"] = base_url
    if token is not None:
        env["APPA_PROVIDER_ARCHESTRA_TOKEN"] = token
    return subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=json.dumps(request),
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )


def answered(completed):
    assert completed.returncode == 0, completed.stderr
    reply = json.loads(completed.stdout)
    assert reply["version"] == 1, reply
    return reply["answer"]


class SelectorTests(unittest.TestCase):
    def test_each_selector_reads_its_collection_as_lowercased_emails(self):
        answers = {
            (("selector", "members"),): (200, {"members": ["Alice@Corp.com", "bob@corp.com"]}),
            (("selector", "team/Platform"),): (200, {"members": ["alice@corp.com"]}),
            (("selector", "user/u-1"),): (200, {"members": ["Carol@Corp.com"]}),
        }
        with RecordedArchestra(answers) as archestra:
            self.assertEqual(
                answered(consult({"selector": "members"}, archestra.base_url)),
                {"members": ["alice@corp.com", "bob@corp.com"]},
            )
            self.assertEqual(
                answered(consult({"selector": "team/Platform"}, archestra.base_url)),
                {"members": ["alice@corp.com"]},
            )
            self.assertEqual(
                answered(consult({"selector": "user/u-1"}, archestra.base_url)),
                {"members": ["carol@corp.com"]},
            )
            self.assertEqual({path for path, _, _ in archestra.seen}, {"/api/openappa/audience"})

    def test_an_unserved_selector_never_reaches_the_api(self):
        with RecordedArchestra({}) as archestra:
            for selector in ["viewer", "team/", "team/a/b", "members/x"]:
                completed = consult({"selector": selector}, archestra.base_url)
                self.assertEqual(completed.returncode, 1, selector)
            self.assertEqual(archestra.seen, [])

    def test_an_api_failure_or_malformed_answer_is_no_answer(self):
        answers = {
            (("selector", "team/gone"),): (404, {"error": "no such team"}),
            (("selector", "team/odd"),): (200, {"members": "alice@corp.com"}),
            (("selector", "team/blank"),): (200, {"members": [""]}),
        }
        with RecordedArchestra(answers) as archestra:
            for team in ["gone", "odd", "blank"]:
                completed = consult({"selector": f"team/{team}"}, archestra.base_url)
                self.assertEqual(completed.returncode, 1, team)
                self.assertEqual(completed.stdout, "")


class MemberLookupTests(unittest.TestCase):
    def test_a_user_id_resolves_to_its_email_or_to_nothing(self):
        answers = {
            (("member", "u-1"),): (200, {"principal": "Alice@Corp.com"}),
            (("member", "u-2"),): (200, {"principal": None}),
            (("member", "u-3"),): (200, {}),
        }
        with RecordedArchestra(answers) as archestra:
            self.assertEqual(
                answered(consult({"member": "archestra:u-1"}, archestra.base_url)),
                {"principal": "alice@corp.com"},
            )
            self.assertEqual(
                answered(consult({"member": "archestra:u-2"}, archestra.base_url)),
                {"principal": None},
            )
            self.assertEqual(consult({"member": "archestra:u-3"}, archestra.base_url).returncode, 1)
            self.assertEqual(consult({"member": "slack:U1"}, archestra.base_url).returncode, 1)


class RequestTests(unittest.TestCase):
    def test_a_declaration_skew_exits_2_before_reading_credentials(self):
        completed = consult({"selector": "members"}, None, token=None, templates=["members"])
        self.assertEqual(completed.returncode, 2)

    def test_missing_configuration_is_no_answer(self):
        with RecordedArchestra({(("selector", "members"),): (200, {"members": []})}) as archestra:
            self.assertEqual(consult({"selector": "members"}, None).returncode, 1)
            self.assertEqual(consult({"selector": "members"}, archestra.base_url, token=None).returncode, 1)
            self.assertEqual(consult({"selector": "members"}, archestra.base_url, token="wrong").returncode, 1)

    def test_an_artifact_names_exactly_a_selector_or_a_member(self):
        with RecordedArchestra({}) as archestra:
            for artifact in [{}, {"selector": "members", "member": "archestra:u-1"}, ["members"]]:
                self.assertEqual(consult(artifact, archestra.base_url).returncode, 1, artifact)


if __name__ == "__main__":
    unittest.main()
