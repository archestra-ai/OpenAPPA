from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
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


PUBLIC = ANNOTATOR.Repository("public", fork=False)
PRIVATE = ANNOTATOR.Repository("private", fork=False)
INTERNAL = ANNOTATOR.Repository("internal", fork=False)
FILE_READ = ANNOTATOR.Call("get_file_contents", None, None)


def member(login, association="MEMBER", bot=False):
    return {"login": login, "association": association, "bot": bot}


def discussion(participants, kind="pull_request", number=12, name="acme/api", **item):
    """A `github` context answer for one pull request or issue, as context.py shapes it."""
    body = {
        "number": number,
        "author": {"login": participants[0]["login"], "association": participants[0]["association"]},
        "locked": False,
        "participants": participants,
        "last_editors": [],
        "truncated": False,
    }
    if kind == "pull_request":
        body |= {"cross_repository": False, "commit_authors": [participants[0]["login"]]}
    return {"answer": {"viewer": "ana", "repository": {"name": name, "visibility": "public"}, kind: body | item}}


def pull_request_read(context, number=12):
    return ANNOTATOR.Call("pull_request_read", number, context)


def listed(number, login, association="MEMBER", bot=False, **edited):
    """One listed item, as context.py shapes it; `last_editor` only when edited."""
    return {"number": number, "author": {"login": login, "association": association}, "bot": bot, **edited}


def listing(kind, items, name="acme/api", truncated=False):
    """A `github` context answer for one listing, as context.py shapes it."""
    return {"answer": {"viewer": "ana", "repository": {"name": name, "visibility": "public"}, kind: {"items": items, "truncated": truncated}}}


def list_issues(context):
    return ANNOTATOR.Call("list_issues", None, context)


def read_trust(call, repository=PUBLIC):
    return ANNOTATOR.annotation(ANNOTATOR.CONTENT, call, repository, "acme", "api")["delta"].get("trust")


class AnswerTests(unittest.TestCase):
    def test_a_read_of_public_repository_content_is_suspicious_and_public(self):
        self.assertEqual(
            ANNOTATOR.annotation(ANNOTATOR.CONTENT, FILE_READ, PUBLIC, "acme", "api"),
            {
                "delta": {"trust": "suspicious", "audience": "public"},
                "requires": {"history": [], "attention": []},
                "emits": [],
            },
        )

    def test_a_read_of_private_or_internal_content_keeps_trust_and_narrows_to_its_collaborators(self):
        for repository in [PRIVATE, INTERNAL]:
            answer = ANNOTATOR.annotation(ANNOTATOR.CONTENT, FILE_READ, repository, "acme", "api")
            self.assertEqual(answer["delta"], {"audience": [COLLABORATORS]})

    def test_a_private_forks_content_came_from_its_parent_and_is_suspicious(self):
        fork = ANNOTATOR.Repository("private", fork=True)
        self.assertEqual(
            ANNOTATOR.annotation(ANNOTATOR.CONTENT, FILE_READ, fork, "acme", "api")["delta"],
            {"trust": "suspicious", "audience": [COLLABORATORS]},
        )

    def test_a_public_pull_request_the_team_alone_wrote_keeps_trust(self):
        context = discussion([member("ana"), member("bo", "COLLABORATOR"), member("cy", "OWNER")])
        answer = ANNOTATOR.annotation(ANNOTATOR.CONTENT, pull_request_read(context), PUBLIC, "acme", "api")
        self.assertEqual(answer["delta"], {"audience": "public"})

    def test_a_team_only_issue_keeps_trust(self):
        call = ANNOTATOR.Call("issue_read", 7, discussion([member("ana")], kind="issue", number=7))
        self.assertIsNone(read_trust(call))

    def test_an_outsider_anywhere_on_the_pull_request_makes_it_suspicious(self):
        team = [member("ana"), member("bo", "COLLABORATOR")]
        for outsider in ["CONTRIBUTOR", "FIRST_TIME_CONTRIBUTOR", "FIRST_TIMER", "NONE", "MANNEQUIN", None]:
            context = discussion([*team, member("mallory", outsider)])
            self.assertEqual(read_trust(pull_request_read(context)), "suspicious", outsider)
        self.assertEqual(read_trust(pull_request_read(discussion([*team, member(None, "NONE")]))), "suspicious")

    def test_an_installed_apps_comment_keeps_trust(self):
        bot = member("cla-bot", "NONE", bot=True)
        self.assertIsNone(read_trust(pull_request_read(discussion([member("ana"), bot]))))

    def test_an_outsiders_comment_on_a_private_repository_is_suspicious_there_too(self):
        context = discussion([member("ana"), member("mallory", "NONE")], kind="issue", number=7)
        answer = ANNOTATOR.annotation(ANNOTATOR.CONTENT, ANNOTATOR.Call("issue_read", 7, context), PRIVATE, "acme", "api")
        self.assertEqual(answer["delta"], {"trust": "suspicious", "audience": [COLLABORATORS]})

    def test_a_commit_author_or_editor_outside_the_team_makes_it_suspicious(self):
        team = [member("ana"), member("bo")]
        for item in [
            {"commit_authors": ["ana", "mallory"]},
            {"commit_authors": ["ana", None]},
            {"last_editors": ["mallory"]},
            {"last_editors": [None]},
        ]:
            self.assertEqual(read_trust(pull_request_read(discussion(team, **item))), "suspicious", item)
        self.assertIsNone(read_trust(pull_request_read(discussion(team, commit_authors=["bo"], last_editors=["ana"]))))

    def test_a_truncated_answer_is_suspicious(self):
        self.assertEqual(read_trust(pull_request_read(discussion([member("ana")], truncated=True))), "suspicious")

    def test_an_error_absent_or_mismatched_context_is_suspicious(self):
        team = [member("ana")]
        for call in [
            pull_request_read(None),
            pull_request_read({"error": "non_success status=1"}),
            pull_request_read({"answer": None}),
            pull_request_read(discussion(team, number=13)),
            pull_request_read(discussion(team, name="acme/other")),
            pull_request_read(discussion(team, kind="issue")),
            pull_request_read(discussion(team), number=None),
            ANNOTATOR.Call("issue_read", 12, discussion(team)),
        ]:
            self.assertEqual(read_trust(call, PRIVATE), "suspicious", call)
        self.assertIsNone(read_trust(pull_request_read(discussion(team, name="Acme/API"))))

    def test_a_team_only_listing_keeps_trust(self):
        for tool, kind in [("list_issues", "issues"), ("list_pull_requests", "pull_requests")]:
            items = [listed(1, "ana"), listed(2, "bo", "COLLABORATOR", last_editor="ana"), listed(3, "cy", "OWNER")]
            self.assertIsNone(read_trust(ANNOTATOR.Call(tool, None, listing(kind, items))), tool)
        self.assertIsNone(read_trust(list_issues(listing("issues", []))))

    def test_an_installed_apps_listed_item_keeps_trust(self):
        items = [listed(1, "ana"), listed(2, "dependabot", "NONE", bot=True, last_editor="dependabot")]
        self.assertIsNone(read_trust(list_issues(listing("issues", items))))

    def test_an_outsiders_listed_item_or_edit_makes_the_listing_suspicious(self):
        team = [listed(1, "ana"), listed(2, "bo")]
        for outsider in [
            listed(3, "mallory", "CONTRIBUTOR"),
            listed(3, "mallory", "NONE"),
            listed(3, None, "NONE"),
            listed(3, None, "MEMBER"),
            listed(3, "ana", "MEMBER", last_editor="mallory"),
            listed(3, "ana", "MEMBER", last_editor=None),
            {"number": 3, "author": "ana", "bot": False},
            "ana",
        ]:
            self.assertEqual(read_trust(list_issues(listing("issues", [*team, outsider])), PRIVATE), "suspicious", outsider)

    def test_a_truncated_absent_or_mismatched_listing_is_suspicious(self):
        team = [listed(1, "ana")]
        for call in [
            list_issues(listing("issues", team, truncated=True)),
            list_issues(listing("issues", team, truncated=None)),
            list_issues(None),
            list_issues({"error": "non_success status=1"}),
            list_issues({"answer": None}),
            list_issues(listing("issues", team, name="acme/other")),
            list_issues(listing("pull_requests", team)),
            list_issues({"answer": {"repository": {"name": "acme/api"}, "issues": {"items": "ana", "truncated": False}}}),
            list_issues(discussion([member("ana")], kind="issue")),
        ]:
            self.assertEqual(read_trust(call, PRIVATE), "suspicious", call)

    def test_only_a_reported_visibility_and_fork_flag_are_answered(self):
        for payload in [
            {"private": False},
            {"visibility": "secret", "fork": False},
            {"visibility": None, "fork": False},
            {"visibility": "private"},
            {"visibility": "private", "fork": None},
            [],
        ]:
            with self.assertRaises(RuntimeError):
                ANNOTATOR.repository_facts(lambda _path: payload, "acme", "api")
        self.assertEqual(
            ANNOTATOR.repository_facts(lambda _path: {"private": False, "visibility": "internal", "fork": True}, "acme", "api"),
            ANNOTATOR.Repository("internal", fork=True),
        )

    def test_a_write_needs_trusted_data_everyone_the_repository_reaches_may_see(self):
        write = ANNOTATOR.Call("add_issue_comment", 7, None)
        self.assertEqual(
            ANNOTATOR.annotation(ANNOTATOR.READERS, write, PUBLIC, "acme", "api")["requires"],
            {"trust": "trusted", "audience": {"contains": "public"}, "history": [], "attention": []},
        )
        self.assertEqual(
            ANNOTATOR.annotation(ANNOTATOR.READERS, write, PRIVATE, "acme", "api")["requires"]["audience"],
            {"contains": [COLLABORATORS]},
        )
        # Every enterprise member reads an internal repository, more than the collaborators.
        self.assertEqual(
            ANNOTATOR.annotation(ANNOTATOR.READERS, write, INTERNAL, "acme", "api")["requires"]["audience"],
            {"contains": "public"},
        )


class ConsultTests(unittest.TestCase):
    def test_the_repository_is_read_from_the_calls_arguments(self):
        self.assertEqual(ANNOTATOR.repository_of(consult()), (ANNOTATOR.CONTENT, "acme", "api", FILE_READ))
        self.assertEqual(ANNOTATOR.repository_of(consult(name=ANNOTATOR.READERS))[0], ANNOTATOR.READERS)

    def test_the_tool_number_and_github_context_are_read_from_the_artifact(self):
        context = discussion([member("ana")])
        artifact = {
            "args": {"name": "mcp/github/pull_request_read", "arguments": {"owner": "acme", "repo": "api", "pullNumber": 12}},
            "context": {"github": context, "other": {"answer": {}}},
        }
        self.assertEqual(ANNOTATOR.repository_of(consult(artifact=artifact))[3], pull_request_read(context))

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
        with Loopback({"/repos/acme/api": {"visibility": "private", "fork": False}}) as github:
            result = self.run_script(consult(), github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(github.seen, [("/repos/acme/api", "Bearer ghp-fixture")])
        self.assertEqual(json.loads(result.stdout)["answer"]["delta"], {"audience": [COLLABORATORS]})

    def test_a_team_only_public_pull_request_read_keeps_trust_end_to_end(self):
        artifact = {
            "args": {"name": "mcp/github/pull_request_read", "arguments": {"owner": "acme", "repo": "api", "pullNumber": 12}},
            "context": {"github": discussion([member("ana"), member("bo", "COLLABORATOR")])},
        }
        with Loopback({"/repos/acme/api": {"visibility": "public", "fork": False}}) as github:
            result = self.run_script(consult(artifact=artifact), github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["answer"]["delta"], {"audience": "public"})

    def test_a_write_carrying_a_large_file_is_answered(self):
        arguments = {"owner": "acme", "repo": "api", "path": "data.txt", "content": "x" * 100_000}
        request = consult(name=ANNOTATOR.READERS, artifact={"args": {"name": "create_or_update_file", "arguments": arguments}})
        with Loopback({"/repos/acme/api": {"visibility": "private", "fork": False}}) as github:
            result = self.run_script(request, github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["answer"]["requires"]["audience"], {"contains": [COLLABORATORS]})

    def test_a_missing_token_is_a_failure_before_any_network(self):
        # A PATH with no gh on it: neither the variable nor a CLI login answers.
        with tempfile.TemporaryDirectory() as empty:
            result = self.run_script(consult(), {"PATH": empty})
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
