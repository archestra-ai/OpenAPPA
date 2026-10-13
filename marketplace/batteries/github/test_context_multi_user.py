from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import threading
import unittest


SCRIPT = Path(__file__).with_name("context-multi-user.py")
SPEC = importlib.util.spec_from_file_location("github_context", Path(__file__).with_name("context.py"))
CONTEXT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTEXT)


def user(number):
    return {"__typename": "User", "databaseId": number}


def bot(number):
    return {"__typename": "Bot", "databaseId": number}


# A mannequin is an imported account's placeholder; the query reads no id for it.
MANNEQUIN = {"__typename": "Mannequin"}


def written(author, association, editor=None):
    return {
        "author": author,
        "authorAssociation": association,
        "editor": editor,
        "lastEditedAt": "2026-09-01T10:00:00Z" if editor else None,
    }


def page(nodes, more=False):
    return {"pageInfo": {"hasNextPage": more}, "nodes": nodes}


# What GitHub returns to the multi-user query for a pull request: no viewer,
# no viewerPermission, every actor by its database id.
PULL_REQUEST_PAYLOAD = {
    "data": {
        "repository": {
            "nameWithOwner": "acme/widget",
            "visibility": "PRIVATE",
            "parent": None,
            "issueOrPullRequest": {
                "__typename": "PullRequest",
                "number": 12,
                "locked": False,
                "isCrossRepository": False,
                **written(user(11), "MEMBER", editor=user(11)),
                "comments": page([written(bot(41), "NONE"), written(MANNEQUIN, "NONE"), written(None, "NONE")]),
                "reviews": page([{**written(user(12), "COLLABORATOR"), "comments": page([written(user(12), "COLLABORATOR", editor=user(13))])}]),
                "commits": page(
                    [
                        {"commit": {"authors": page([{"user": {"databaseId": 11}}])}},
                        {"commit": {"authors": page([{"user": None}, {"user": {"databaseId": 14}}])}},
                    ]
                ),
            },
        }
    }
}

ISSUES_PAYLOAD = {
    "data": {
        "repository": {
            "nameWithOwner": "acme/widget",
            "visibility": "PUBLIC",
            "parent": None,
            "issues": page(
                [
                    {"number": 41, **written(user(11), "MEMBER", editor=user(12))},
                    {"number": 40, **written(bot(41), "NONE")},
                ]
            ),
        }
    }
}


class Loopback:
    """One stdlib HTTP server standing in for GitHub's GraphQL endpoint."""

    def __init__(self, answer):
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                seen.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                encoded = json.dumps(answer).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
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


def consult(arguments, tool="mcp/github/pull_request_read"):
    return {"version": 1, "kind": "context", "name": "github", "declaration": {}, "artifact": {"tool": tool, "arguments": arguments}}


def run_script(request, env):
    return subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(request), capture_output=True, text=True, env=env)


def selected_fields(query):
    """Every field name the GraphQL query selects, fragments' type names excluded."""
    words = query.replace("{", " ").replace("}", " ").replace("(", " ( ").replace(")", " ) ").split()
    fields, depth = set(), 0
    for word in words:
        match word:
            case "(":
                depth += 1
            case ")":
                depth -= 1
            case _ if depth == 0 and word.isidentifier():
                fields.add(word)
    return fields


class MultiUserAnswers(unittest.TestCase):
    def test_a_pull_request_names_every_person_by_numeric_id(self):
        with Loopback(PULL_REQUEST_PAYLOAD) as github:
            result = run_script(consult({"owner": "acme", "repo": "widget", "pullNumber": 12}), github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout)["answer"],
            {
                "repository": {"name": "acme/widget", "visibility": "private", "fork_of": None},
                "pull_request": {
                    "number": 12,
                    "author": {"person": "github:11", "association": "MEMBER"},
                    "locked": False,
                    "cross_repository": False,
                    "participants": [
                        {"person": "github:11", "association": "MEMBER", "bot": False},
                        {"person": "github:41", "association": "NONE", "bot": True},
                        {"person": None, "association": "NONE", "bot": False},
                        {"person": "github:12", "association": "COLLABORATOR", "bot": False},
                    ],
                    "commit_authors": ["github:11", None, "github:14"],
                    "last_editors": ["github:11", "github:13"],
                    "truncated": False,
                },
            },
        )
        [request] = github.seen
        self.assertEqual(request["variables"], CONTEXT.variables_of("acme/widget", 12))
        fields = selected_fields(request["query"])
        self.assertTrue({"viewer", "viewerPermission", "login"}.isdisjoint(fields), fields)
        self.assertIn("databaseId", fields)

    def test_a_listing_names_each_author_and_editor_by_numeric_id(self):
        with Loopback(ISSUES_PAYLOAD) as github:
            result = run_script(consult({"owner": "acme", "repo": "widget", "state": "OPEN"}, "mcp/github/list_issues"), github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout)["answer"]["issues"],
            {
                "items": [
                    {"number": 41, "author": {"person": "github:11", "association": "MEMBER"}, "bot": False, "last_editor": "github:12"},
                    {"number": 40, "author": {"person": "github:41", "association": "NONE"}, "bot": True},
                ],
                "truncated": False,
            },
        )

    def test_the_single_user_query_still_reads_the_viewer_and_logins(self):
        fields = selected_fields(CONTEXT.query_of(CONTEXT.Deployment.SINGLE_USER))
        self.assertTrue({"viewer", "viewerPermission", "login"} <= fields, fields)
        self.assertNotIn("databaseId", fields)

    def test_the_token_comes_from_the_deployment_never_from_gh(self):
        with tempfile.TemporaryDirectory() as directory:
            record = Path(directory) / "argv"
            gh = Path(directory) / "gh"
            gh.write_text(f"#!/bin/sh\necho called > {record}\necho gho-someones-login\n")
            gh.chmod(gh.stat().st_mode | stat.S_IXUSR)
            with Loopback(PULL_REQUEST_PAYLOAD) as github:
                result = run_script(
                    consult({"owner": "acme", "repo": "widget", "pullNumber": 12}),
                    github.env(PATH=f"{directory}:/usr/bin:/bin", APPA_PROVIDER_GITHUB_TOKEN=""),
                )
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stdout, "")
            self.assertFalse(record.exists())
            self.assertEqual(github.seen, [])


if __name__ == "__main__":
    unittest.main()
