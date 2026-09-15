from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import threading
import unittest


SCRIPT = Path(__file__).with_name("audience-source.py")
SPEC = importlib.util.spec_from_file_location("audience_source", SCRIPT)
AUDIENCE_SOURCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIENCE_SOURCE)

# Recorded Hub payloads (2026-09-15), trimmed to the fields the script reads.
WHOAMI = {"type": "user", "name": "arsenyinfo", "email": "me@arseny.info", "emailVerified": True, "orgs": []}
# A resource group following the Hub's OpenAPI schema; the test token belongs to no organization.
RESEARCH = {
    "id": "507f1f77bcf86cd799439011",
    "name": "research",
    "users": [
        {"type": "user", "name": "alice", "role": "read"},
        {"type": "user", "name": "arsenyinfo", "role": "admin"},
        {"type": "user", "name": "mallory", "role": "no_access"},
    ],
    "resources": [{"type": "model", "name": "acme/weights", "private": True}],
}


def fixture_hub(**answers):
    """A Hub answering from recorded payloads; a path it lacks is a 404."""
    table = {"/api/whoami-v2": WHOAMI, "/api/organizations/acme/resource-groups": [RESEARCH], **answers}

    def call(path):
        match table.get(path):
            case None:
                raise AUDIENCE_SOURCE.NotFound(path)
            case Exception() as error:
                raise error
            case payload:
                return payload

    return call


class SelectorTests(unittest.TestCase):
    def test_viewer_is_the_tokens_own_verified_email(self):
        self.assertEqual(AUDIENCE_SOURCE.answer(fixture_hub(), {"selector": "viewer"}), {"members": ["me@arseny.info"]})

    def test_a_viewer_without_a_verified_email_keeps_the_qualified_id(self):
        for account in [{**WHOAMI, "emailVerified": False}, {**WHOAMI, "email": None}, {"name": "arsenyinfo"}]:
            call = fixture_hub(**{"/api/whoami-v2": account})
            self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}), {"members": ["huggingface:arsenyinfo"]})

    def test_a_resource_group_is_its_users_minus_no_access_by_id_or_name(self):
        for group in ["507f1f77bcf86cd799439011", "research"]:
            answer = AUDIENCE_SOURCE.answer(fixture_hub(), {"selector": f"org/acme/resource-group/{group}/members"})
            self.assertEqual(answer, {"members": ["huggingface:alice", "huggingface:arsenyinfo"]}, group)

    def test_a_group_the_token_cannot_see_is_a_failure_not_an_empty_answer(self):
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            AUDIENCE_SOURCE.answer(fixture_hub(), {"selector": "org/acme/resource-group/other/members"})
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            AUDIENCE_SOURCE.answer(fixture_hub(), {"selector": "org/typo-org/resource-group/research/members"})
        forbidden = fixture_hub(**{"/api/organizations/acme/resource-groups": AUDIENCE_SOURCE.Forbidden("groups")})
        with self.assertRaises(AUDIENCE_SOURCE.Forbidden):
            AUDIENCE_SOURCE.answer(forbidden, {"selector": "org/acme/resource-group/research/members"})

    def test_a_malformed_or_oversized_group_is_refused(self):
        for listing in [{"not": "a list"}, [{**RESEARCH, "users": None}], [{**RESEARCH, "users": [{"role": "read"}]}], [{**RESEARCH, "users": [{"name": "mallory"}]}], [{**RESEARCH, "users": [{"name": "mallory", "role": None}]}], [{**RESEARCH, "users": [{"name": f"u{i}", "role": "read"} for i in range(1001)]}]]:
            call = fixture_hub(**{"/api/organizations/acme/resource-groups": listing})
            with self.assertRaises(RuntimeError, msg=repr(listing)[:60]):
                AUDIENCE_SOURCE.answer(call, {"selector": "org/acme/resource-group/research/members"})

    def test_an_unserved_selector_is_refused(self):
        for selector in ["org/acme/members", "org//resource-group/x/members", "org/acme/resource-group//members", "members", "", 3]:
            with self.assertRaises(ValueError, msg=repr(selector)):
                AUDIENCE_SOURCE.answer(fixture_hub(), {"selector": selector})


class MemberTests(unittest.TestCase):
    def test_the_viewers_own_name_resolves_to_the_viewer(self):
        self.assertEqual(AUDIENCE_SOURCE.answer(fixture_hub(), {"member": "huggingface:arsenyinfo"}), {"principal": "me@arseny.info"})

    def test_another_account_stays_qualified_when_the_hub_knows_it(self):
        call = fixture_hub(**{"/api/users/alice/overview": {"user": "alice", "type": "user"}})
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "huggingface:alice"}), {"principal": "huggingface:alice"})

    def test_an_account_the_hub_does_not_know_is_definitively_none(self):
        self.assertEqual(AUDIENCE_SOURCE.answer(fixture_hub(), {"member": "huggingface:nobody"}), {"principal": None})

    def test_a_foreign_member_is_refused(self):
        for member in ["github:alice", "huggingface:", "alice@corp.example"]:
            with self.assertRaises(ValueError, msg=member):
                AUDIENCE_SOURCE.answer(fixture_hub(), {"member": member})

    def test_an_artifact_must_carry_exactly_a_selector_or_a_member(self):
        for artifact in [{}, {"selector": "viewer", "member": "huggingface:alice"}, {"other": 1}, []]:
            with self.assertRaises(ValueError, msg=repr(artifact)):
                AUDIENCE_SOURCE.answer(fixture_hub(), artifact)


class Loopback:
    """One stdlib HTTP server standing in for the Hub."""

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
        return {"PATH": "/usr/bin:/bin", "APPA_PROVIDER_HUGGINGFACE_TOKEN": "hf-fixture", "HF_ENDPOINT": f"http://127.0.0.1:{self.server.server_port}"}


def request(artifact, **overrides):
    return {
        "version": 1,
        "kind": "audience",
        "name": "huggingface",
        "declaration": {"templates": AUDIENCE_SOURCE.SERVED_TEMPLATES},
        "artifact": artifact,
        **overrides,
    }


class EnvelopeTests(unittest.TestCase):
    def run_script(self, body, env):
        return subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(body), capture_output=True, text=True, env=env)

    def test_the_hub_root_is_hf_endpoint_and_the_token_is_sent(self):
        with Loopback({"/api/whoami-v2": WHOAMI}) as hub:
            result = self.run_script(request({"selector": "viewer"}), hub.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(hub.seen, [("/api/whoami-v2", "Bearer hf-fixture")])
        self.assertEqual(json.loads(result.stdout), {"version": 1, "answer": {"members": ["me@arseny.info"]}})

    def test_a_foreign_envelope_is_refused_before_any_network(self):
        for body in [request({"selector": "viewer"}, version=2), request({"selector": "viewer"}, kind="annotation"), request({"selector": "viewer"}, name="github")]:
            result = self.run_script(body, {"PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_a_declaration_of_other_templates_is_refused_before_the_token_is_read(self):
        for templates in [["viewer"], ["viewer", "org/<org>/members"], None]:
            result = self.run_script(request({"selector": "viewer"}, declaration={"templates": templates}), {"PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
