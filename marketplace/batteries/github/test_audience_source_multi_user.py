from http.server import BaseHTTPRequestHandler, HTTPServer
import json
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import threading
import unittest


SCRIPT = Path(__file__).with_name("audience-source-multi-user.py")

TEMPLATES = ["org/<org>/members", "org/<org>/team/<team>", "repo/<owner>/<repo>/collaborators"]


class Loopback:
    """One stdlib HTTP server standing in for a GitHub API root; a path it
    holds no answer for is a server error, so any read the script was not
    meant to make fails the consult."""

    def __init__(self, answers):
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                seen.append(self.path)
                status, body = (200, json.dumps(answers[self.path]).encode()) if self.path in answers else (500, b"{}")
                self.send_response(status)
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

    def env(self, **extra):
        return {
            "PATH": "/usr/bin:/bin",
            "APPA_PROVIDER_GITHUB_TOKEN": "ghs-installation",
            "GITHUB_API_URL": f"http://127.0.0.1:{self.server.server_port}",
            **extra,
        }


def account(login, number, type="User"):
    return {"login": login, "id": number, "type": type}


def envelope(**overrides):
    return {
        "version": 1,
        "kind": "audience",
        "name": "github",
        "declaration": {"templates": TEMPLATES},
        "artifact": {"selector": "org/acme/members"},
        **overrides,
    }


def run_script(request, env):
    return subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(request), capture_output=True, text=True, env=env)


class Selectors(unittest.TestCase):
    def test_members_are_numeric_ids_from_the_listing_alone_bots_excluded(self):
        first_page = [account(f"member-{index}", 1000 + index) for index in range(100)]
        answers = {
            "/orgs/acme/members?per_page=100&page=1": first_page,
            "/orgs/acme/members?per_page=100&page=2": [account("alice", 7), account("ci-robot", 8, "Bot")],
        }
        with Loopback(answers) as github:
            result = run_script(envelope(), github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        members = json.loads(result.stdout)["answer"]["members"]
        self.assertEqual(members, [f"github:{1000 + index}" for index in range(100)] + ["github:7"])
        self.assertEqual(github.seen, list(answers))

    def test_a_team_and_a_repositorys_collaborators_are_their_own_listings(self):
        for selector, path in [
            ("org/acme/team/finance", "/orgs/acme/teams/finance/members?per_page=100&page=1"),
            ("repo/acme/api/collaborators", "/repos/acme/api/collaborators?per_page=100&page=1"),
        ]:
            with Loopback({path: [account("alice", 7), account("dependabot", 9, "Bot")]}) as github:
                result = run_script(envelope(artifact={"selector": selector}), github.env())
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["answer"], {"members": ["github:7"]})
            self.assertEqual(github.seen, [path])

    def test_an_account_without_a_numeric_id_is_a_failure_not_a_guess(self):
        for listed in [{"login": "alice", "type": "User"}, account("alice", "7"), account("alice", True)]:
            with Loopback({"/orgs/acme/members?per_page=100&page=1": [listed]}) as github:
                result = run_script(envelope(), github.env())
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_a_malformed_listing_is_a_failure_not_a_partial_roster(self):
        for listed in [{}, [{"login": "alice", "id": 7}], [account("alice", 7, "Organization")], ["alice"]]:
            with Loopback({"/orgs/acme/members?per_page=100&page=1": listed}) as github:
                result = run_script(envelope(), github.env())
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_a_collection_past_the_page_budget_is_refused_not_truncated(self):
        budget = 50
        answers = {
            f"/orgs/acme/members?per_page=100&page={page}": [
                account(f"member-{page}-{index}", page * 1000 + index) for index in range(100)
            ]
            for page in range(1, budget + 2)
        }
        with Loopback(answers) as github:
            result = run_script(envelope(), github.env())
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(github.seen, list(answers)[:budget])

    def test_the_viewer_and_unserved_selectors_are_refused_before_any_network(self):
        for selector in ["viewer", "org//members", "org/a/repos", "repo/a/collaborators", ""]:
            with Loopback({}) as github:
                result = run_script(envelope(artifact={"selector": selector}), github.env())
            self.assertEqual(result.returncode, 1, selector)
            self.assertEqual(github.seen, [])

    def test_a_member_lookup_is_refused_before_any_network(self):
        with Loopback({}) as github:
            result = run_script(envelope(artifact={"member": "github:alice"}), github.env())
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertEqual(github.seen, [])


class Envelope(unittest.TestCase):
    def test_a_declaration_other_than_the_served_templates_is_refused(self):
        for templates in [["viewer", *TEMPLATES], TEMPLATES[1:], list(reversed(TEMPLATES)), []]:
            result = run_script(envelope(declaration={"templates": templates}), {"PATH": "/usr/bin:/bin"})
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stdout, "")

    def test_the_token_comes_from_the_deployment_never_from_gh(self):
        with tempfile.TemporaryDirectory() as directory:
            record = Path(directory) / "argv"
            gh = Path(directory) / "gh"
            gh.write_text(f"#!/bin/sh\necho called > {record}\necho gho-someones-login\n")
            gh.chmod(gh.stat().st_mode | stat.S_IXUSR)
            with Loopback({}) as github:
                result = run_script(envelope(), github.env(PATH=f"{directory}:/usr/bin:/bin", APPA_PROVIDER_GITHUB_TOKEN=""))
            self.assertEqual(result.returncode, 1)
            self.assertFalse(record.exists())
            self.assertEqual(github.seen, [])


if __name__ == "__main__":
    unittest.main()
