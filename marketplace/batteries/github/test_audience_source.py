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


def fixture_api(responses):
    """A call answering from recorded GitHub REST payloads, in order."""

    remaining = list(responses)
    # Profile reads arrive from a pool of threads.
    lock = threading.Lock()

    def call(path, **params):
        with lock:
            for index, (fixture_path, fixture_params, response) in enumerate(remaining):
                if fixture_path == path and fixture_params == params:
                    remaining.pop(index)
                    if isinstance(response, Exception):
                        raise response
                    return response
        raise AssertionError(f"unexpected call {path} {params}")

    return call


def user(login, type="User"):
    return {"login": login, "type": type}


def profile(login, email=None):
    """The `/users/{login}` fixture: GitHub reports an unpublished email as null."""
    return (f"/users/{login}", {}, {"login": login, "type": "User", "email": email})


class SelectorTests(unittest.TestCase):
    def test_viewer_is_the_tokens_own_verified_primary_email(self):
        call = fixture_api(
            [
                ("/user", {}, user("alice")),
                (
                    "/user/emails",
                    {},
                    [
                        {"email": "old@corp.com", "primary": False, "verified": True},
                        {"email": "alice@gmail.com", "primary": True, "verified": True},
                        {"email": "spoof@corp.com", "primary": False, "verified": False},
                    ],
                ),
            ]
        )
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}), {"members": ["alice@gmail.com"]})

    def test_a_viewer_without_readable_emails_keeps_the_qualified_id(self):
        call = fixture_api(
            [
                ("/user", {}, user("alice")),
                ("/user/emails", {}, AUDIENCE_SOURCE.Forbidden("/user/emails")),
            ]
        )
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"selector": "viewer"}), {"members": ["github:alice"]})

    def test_org_members_are_profile_emails_or_qualified_ids_bots_excluded(self):
        first_page = [user(f"member-{index}") for index in range(100)]
        call = fixture_api(
            [
                ("/orgs/archestra-ai/members", {"per_page": 100, "page": 1}, first_page),
                (
                    "/orgs/archestra-ai/members",
                    {"per_page": 100, "page": 2},
                    [user("alice"), user("bob"), user("ci-robot", type="Bot")],
                ),
                *[profile(f"member-{index}") for index in range(100)],
                profile("alice", "alice@corp.com"),
                profile("bob", ""),
            ]
        )
        answer = AUDIENCE_SOURCE.answer(call, {"selector": "org/archestra-ai/members"})
        self.assertEqual(len(answer["members"]), 102)
        self.assertIn("alice@corp.com", answer["members"])
        self.assertIn("github:bob", answer["members"])
        self.assertIn("github:member-7", answer["members"])
        self.assertNotIn("github:ci-robot", answer["members"])
        self.assertNotIn("github:alice", answer["members"])

    def test_a_team_reports_its_own_membership(self):
        call = fixture_api(
            [
                (
                    "/orgs/archestra-ai/teams/finance/members",
                    {"per_page": 100, "page": 1},
                    [user("alice"), user("bob")],
                ),
                profile("alice", "alice@corp.com"),
                profile("bob"),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "org/archestra-ai/team/finance"}),
            {"members": ["alice@corp.com", "github:bob"]},
        )

    def test_a_member_whose_profile_cannot_be_read_is_a_failure_not_a_guess(self):
        call = fixture_api(
            [
                ("/orgs/archestra-ai/teams/finance/members", {"per_page": 100, "page": 1}, [user("alice")]),
                ("/users/alice", {}, AUDIENCE_SOURCE.NotFound("/users/alice")),
            ]
        )
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            AUDIENCE_SOURCE.answer(call, {"selector": "org/archestra-ai/team/finance"})

    def test_an_unknown_org_or_team_is_a_failure_not_an_empty_answer(self):
        call = fixture_api(
            [
                (
                    "/orgs/typo-org/members",
                    {"per_page": 100, "page": 1},
                    AUDIENCE_SOURCE.NotFound("/orgs/typo-org/members"),
                )
            ]
        )
        with self.assertRaises(AUDIENCE_SOURCE.NotFound):
            AUDIENCE_SOURCE.answer(call, {"selector": "org/typo-org/members"})

    def test_a_repositorys_collaborators_are_its_own_membership(self):
        call = fixture_api(
            [
                (
                    "/repos/archestra-ai/OpenAPPA/collaborators",
                    {"per_page": 100, "page": 1},
                    [user("alice"), user("bob"), user("ci-robot", type="Bot")],
                ),
                profile("alice", "alice@corp.com"),
                profile("bob"),
            ]
        )
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"selector": "repo/archestra-ai/OpenAPPA/collaborators"}),
            {"members": ["alice@corp.com", "github:bob"]},
        )

    def test_a_collection_over_the_bound_is_refused_before_any_profile_is_read(self):
        pages = [
            ("/orgs/acme/members", {"per_page": 100, "page": page}, [user(f"u{page}-{i}") for i in range(100)])
            for page in range(1, 12)
        ]
        with self.assertRaises(RuntimeError):
            AUDIENCE_SOURCE.answer(fixture_api(pages), {"selector": "org/acme/members"})

    def test_an_unserved_selector_is_refused(self):
        call = fixture_api([])
        for selector in [
            "full-members",
            "org//members",
            "org/a/team/",
            "org/a/repos",
            "",
            "repo/a/collaborators",
            "repo//b/collaborators",
            "repo/a/b/members",
        ]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"selector": selector})


class MemberLookupTests(unittest.TestCase):
    def test_a_member_with_a_published_email_resolves_to_it(self):
        call = fixture_api([profile("Alice", "alice@corp.com")])
        self.assertEqual(
            AUDIENCE_SOURCE.answer(call, {"member": "github:Alice"}),
            {"principal": "alice@corp.com"},
        )

    def test_a_member_without_a_published_email_is_the_reader_as_written(self):
        call = fixture_api([("/users/alice", {}, {"login": "Alice", "type": "User", "email": None})])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "github:alice"}), {"principal": "github:alice"})

    def test_an_unknown_member_is_a_definitive_null(self):
        call = fixture_api([("/users/ghost", {}, AUDIENCE_SOURCE.NotFound("/users/ghost"))])
        self.assertEqual(AUDIENCE_SOURCE.answer(call, {"member": "github:ghost"}), {"principal": None})

    def test_a_foreign_or_bare_member_spelling_is_refused(self):
        call = fixture_api([])
        for member in ["slack:U1", "github:", "alice", ""]:
            with self.assertRaises(ValueError):
                AUDIENCE_SOURCE.answer(call, {"member": member})


class EnvelopeTests(unittest.TestCase):
    def run_script(self, request, env):
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=json.dumps(request),
            capture_output=True,
            text=True,
            env=env,
        )

    def envelope(self, **overrides):
        return {
            "version": 1,
            "kind": "audience",
            "name": "github",
            "declaration": {
                "templates": ["viewer", "org/<org>/members", "org/<org>/team/<team>", "repo/<owner>/<repo>/collaborators"]
            },
            "artifact": {"selector": "viewer"},
            **overrides,
        }

    def test_a_foreign_envelope_is_refused(self):
        env = {"PATH": "/usr/bin:/bin", "APPA_PROVIDER_GITHUB_TOKEN": "ghp-fixture"}
        for request in [
            self.envelope(version=2),
            self.envelope(kind="annotation"),
            self.envelope(name="slack"),
        ]:
            result = self.run_script(request, env)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_the_api_root_is_github_api_url(self):
        answers = {"/user": {"login": "octocat", "id": 1}, "/user/emails": [{"email": "octocat@example.com", "primary": True, "verified": True}]}
        with Loopback(answers) as github:
            result = self.run_script(self.envelope(), github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([path for path, _ in github.seen], ["/user", "/user/emails"])
        self.assertEqual({token for _, token in github.seen}, {"Bearer ghp-fixture"})
        self.assertEqual(json.loads(result.stdout)["answer"], {"members": ["octocat@example.com"]})

    def test_a_missing_token_is_a_failure_before_any_network(self):
        result = self.run_script(self.envelope(), {"PATH": "/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("APPA_PROVIDER_GITHUB_TOKEN", result.stderr)

    def test_a_foreign_declaration_is_refused_before_the_token_is_read(self):
        declared = self.envelope()["declaration"]["templates"]
        for templates in [declared + ["channel/<id>"], declared[1:], [], list(reversed(declared))]:
            result = self.run_script(self.envelope(declaration={"templates": templates}), {"PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
