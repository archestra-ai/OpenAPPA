from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest


SCRIPT = Path(__file__).with_name("context.py")
SPEC = importlib.util.spec_from_file_location("github_context", SCRIPT)
CONTEXT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTEXT)
Target = CONTEXT.Target
Listing = CONTEXT.Listing
PREFIX, WHOLE, UNKNOWN = CONTEXT.PREFIX, CONTEXT.WHOLE, CONTEXT.UNKNOWN


def bash(command, cwd=None, tool="host/claude-code/Bash"):
    artifact = {"tool": tool, "arguments": {"command": command}}
    if cwd is not None:
        artifact["cwd"] = cwd
    return artifact


def consult(artifact):
    return {"version": 1, "kind": "context", "name": "github", "declaration": {}, "artifact": artifact}


def slug(name, number=None, directory="/w"):
    return [Target("slug", name, directory, number)]


def checkout(number=None, directory="/w"):
    return [Target("checkout", None, directory, number)]


def actor(login, typename="User"):
    return {"login": login, "__typename": typename}


def written(login, association, typename="User", editor=None):
    return {
        "author": actor(login, typename) if login else None,
        "authorAssociation": association,
        "editor": {"login": editor} if editor else None,
        "lastEditedAt": "2026-09-01T10:00:00Z" if editor else None,
    }


def page(nodes, more=False):
    return {"pageInfo": {"hasNextPage": more}, "nodes": nodes}


# A recorded `issueOrPullRequest` response for a public repository's pull
# request: a member's pull request, a collaborator's review with an inline
# comment, a bot's comment, and a commit by a git identity with no account.
PULL_REQUEST_PAYLOAD = {
    "data": {
        "viewer": {"login": "ana"},
        "repository": {
            "nameWithOwner": "acme/widget",
            "visibility": "PUBLIC",
            "viewerPermission": "ADMIN",
            "parent": None,
            "issueOrPullRequest": {
                "__typename": "PullRequest",
                "number": 12,
                "locked": False,
                "isCrossRepository": False,
                **written("ana", "MEMBER", editor="ana"),
                "comments": page([written("renovate", "NONE", "Bot"), written("ana", "MEMBER")]),
                "reviews": page(
                    [
                        {
                            **written("bo", "COLLABORATOR"),
                            "comments": page([written("bo", "COLLABORATOR", editor="cy")]),
                        }
                    ]
                ),
                "commits": page(
                    [
                        {"commit": {"authors": page([{"user": {"login": "ana"}}])}},
                        {"commit": {"authors": page([{"user": None}, {"user": {"login": "ana"}}])}},
                    ]
                ),
            },
        },
    }
}

ISSUE_PAYLOAD = {
    "data": {
        "viewer": {"login": "ana"},
        "repository": {
            "nameWithOwner": "acme/billing",
            "visibility": "PRIVATE",
            "viewerPermission": "WRITE",
            "parent": {"nameWithOwner": "upstream/billing"},
            "issueOrPullRequest": {
                "__typename": "Issue",
                "number": 7,
                "locked": True,
                **written(None, "NONE"),
                "comments": page([written("ana", "MEMBER")], more=True),
            },
        },
    }
}


class Loopback:
    """One stdlib HTTP server standing in for GitHub's GraphQL endpoint."""

    def __init__(self, status, answer):
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                seen.append((self.path, self.headers.get("Authorization"), body["variables"]))
                encoded = json.dumps(answer).encode()
                self.send_response(status)
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

    def env(self):
        return {"PATH": "/usr/bin:/bin", "APPA_PROVIDER_GITHUB_TOKEN": "ghp-fixture", "GITHUB_API_URL": f"http://127.0.0.1:{self.server.server_port}"}


class CallsThatReachNothing(unittest.TestCase):
    def test_a_call_that_names_no_github_place_has_no_target(self):
        for artifact in [
            {"tool": "host/claude-code/Read", "arguments": {"file_path": "README.md"}},
            {"tool": "fetch", "arguments": {"url": "https://github.com/acme/api/pull/1"}},
            {"tool": "mcp/github/get_me", "arguments": {}},
            {"tool": "mcp/github/search_code", "arguments": {"query": "repo:acme/api retry"}},
            {"tool": "mcp/github/create_repository", "arguments": {"name": "new"}},
            {"tool": "mcp/gitlab/get_file_contents", "arguments": {"owner": "acme", "repo": "api"}},
            {"tool": "mcp/github/get_file_contents", "arguments": {"owner": "acme/x", "repo": "api"}},
            {"tool": "host/claude-code/Bash", "arguments": {}},
            {"tool": "host/claude-code/Bash", "arguments": "gh pr view 1"},
            bash("ls -la && cat README.md"),
            bash("echo github.com"),
            bash("git status && git log --oneline | head -5"),
            bash("git commit -m 'fix' && git diff HEAD~1"),
            bash("gh auth status"),
            bash("gh --version"),
            bash("git push https://gitlab.com/acme/api.git main"),
            bash("(cd sub && ls)"),
        ]:
            self.assertEqual(CONTEXT.call_targets(artifact), [], artifact)
            self.assertIsNone(CONTEXT.context_of(consult(artifact)), artifact)

    def test_a_foreign_consult_is_refused(self):
        for request in [{"version": 2, "kind": "context"}, {"version": 1, "kind": "annotation"}, {"version": 1, "kind": "context"}, []]:
            with self.assertRaises(ValueError):
                CONTEXT.context_of(request)


class McpTargets(unittest.TestCase):
    def test_a_github_tool_names_its_repository_and_item(self):
        for tool, arguments, expected in [
            ("get_file_contents", {"owner": "acme", "repo": "api", "path": "x"}, None),
            ("pull_request_read", {"method": "get_comments", "owner": "acme", "repo": "api", "pullNumber": 12}, 12),
            ("issue_read", {"method": "get", "owner": "acme", "repo": "api", "issue_number": 7}, 7),
            ("add_issue_comment", {"owner": "acme", "repo": "api", "issue_number": 7, "body": "x"}, 7),
            ("merge_pull_request", {"owner": "acme", "repo": "api", "pullNumber": 3}, 3),
            ("pull_request_read", {"owner": "acme", "repo": "api", "pullNumber": True}, None),
            ("pull_request_read", {"owner": "acme", "repo": "api", "pullNumber": "12"}, None),
        ]:
            artifact = {"tool": f"mcp/github/{tool}", "arguments": arguments}
            self.assertEqual(CONTEXT.call_targets(artifact), slug("acme/api", expected, None), artifact)


class GhTargets(unittest.TestCase):
    def targets(self, command, cwd="/w"):
        return CONTEXT.bash_targets(command, cwd)

    def test_a_gh_call_chooses_its_repository_by_flag_or_environment(self):
        for command in (
            "gh pr create --repo acme/api --title x",
            "gh issue create -R acme/api",
            "gh issue create -Racme/api",
            "gh issue create -R=acme/api",
            "gh pr create --repo=acme/api",
            "GH_REPO=acme/api gh pr create",
            "export GH_REPO=acme/api; gh pr create",
            "sudo GH_REPO=acme/api gh pr create",
            "gh repo view acme/api",
            "gh repo edit acme/api --description x",
            "gh repo view https://github.com/acme/api",
            "gh api repos/acme/api/issues -f title=x",
            "gh api /repos/acme/api/pulls",
            "gh api https://api.github.com/repos/acme/api/pulls",
            "gh api -X POST repos/acme/api/labels -f name=bug",
        ):
            self.assertEqual(self.targets(command), slug("acme/api"), command)

    def test_a_pull_request_or_issue_is_named_by_number_or_url(self):
        for command, expected in [
            ("gh pr view 12 --comments", checkout(12)),
            ("gh pr view '#12'", checkout(12)),
            ("gh pr view --comments 12", checkout(12)),
            ("gh pr view --json title,body 12", checkout(12)),
            ("gh pr diff 12 | head -50", checkout(12)),
            ("gh pr checks 12 -R acme/api", slug("acme/api", 12)),
            ("gh pr comment 12 --body 'looks good'", checkout(12)),
            ("gh pr merge 12 --squash --delete-branch", checkout(12)),
            ("gh issue view 7 --comments", checkout(7)),
            ("gh issue comment https://github.com/acme/api/issues/5 --body x", slug("acme/api", 5)),
            ("gh pr view https://github.com/acme/api/pull/12", slug("acme/api", 12)),
            ("gh pr view https://github.com/acme/api/pull/12 -R acme/other", slug("acme/api", 12)),
            ("gh api repos/acme/api/pulls/12/comments", slug("acme/api", 12)),
            ("gh api repos/acme/api/issues/7/comments --paginate", slug("acme/api", 7)),
            ("gh api repos/{owner}/{repo}/pulls/12/reviews", checkout(12)),
        ]:
            self.assertEqual(self.targets(command), expected, command)

    def test_a_call_without_a_number_reaches_the_repository_alone(self):
        for command in (
            "gh pr create --title 'Fix the retry loop' --body-file notes.md",
            "gh pr view",
            "gh pr view feature-branch",
            "gh pr view #12",
            "gh release list",
            "gh pr create --draft",
            'gh pr create --body "see https://github.com/acme/public"',
            "gh pr create --body https://example.com/notes",
            "gh pr create --title --repo acme/api",
            "gh api repos/{owner}/{repo}/pulls",
        ):
            self.assertEqual(self.targets(command), checkout(), command)

    def test_a_cd_moves_every_later_call_to_its_directory(self):
        self.assertEqual(self.targets("cd /other; gh pr view 3"), checkout(3, "/other"))
        self.assertEqual(self.targets("cd sub && gh pr view 3"), checkout(3, "/w/sub"))


class GitTargets(unittest.TestCase):
    def targets(self, command, cwd="/w"):
        return CONTEXT.bash_targets(command, cwd)

    def test_a_push_names_a_url_or_a_remote_in_its_directory(self):
        for command in (
            "git push https://github.com/acme/api.git main",
            "git push git@github.com:acme/api.git",
            "git push --repo https://github.com/acme/api.git",
            "git push --repo=git@github.com:acme/api.git",
        ):
            self.assertEqual(self.targets(command), slug("acme/api"), command)
        for command in (
            "git push --repo upstream",
            "cd /w && git push -u upstream feat 2>&1 | tail -2",
            "  git push upstream",
            "git status\ngit push upstream",
            "sudo git push upstream",
            "git push -o ci.skip upstream main",
            "git --no-pager push upstream",
        ):
            self.assertEqual(self.targets(command), [Target("remote", "upstream", "/w")], command)
        self.assertEqual(self.targets("git -C sub push origin"), [Target("remote", "origin", "/w/sub")])
        self.assertEqual(self.targets("git -C /other push origin"), [Target("remote", "origin", "/other")])
        self.assertEqual(self.targets("git -Csub push origin"), [Target("remote", "origin", "/w/sub")])

    def test_a_bare_push_goes_to_the_branchs_remote(self):
        for command in (
            "git push",
            "git push -u --force-with-lease",
            "ls -R src && git push",
            "git -c color.ui=never status && git push",
            "git remote -v && git push",
            "git commit -m \"$(cat <<'EOF'\nfix the parser\nEOF\n)\" && git push",
        ):
            self.assertEqual(self.targets(command), [Target("push", None, "/w")], command)

    def test_every_destination_of_a_compound_command_is_named(self):
        command = "git push https://github.com/acme/public.git && gh pr create --repo acme/private"
        self.assertEqual(self.targets(command, None), slug("acme/public", None, None) + slug("acme/private", None, None))


class Unfollowable(unittest.TestCase):
    def test_a_command_this_provider_cannot_follow_is_refused(self):
        for command in (
            'bash -c "git push https://github.com/acme/public.git"',
            "sudo -u bob git push upstream",
            "bash deploy.sh && git push",
            "fish -c 'gh pr create --repo acme/public'",
            "V=git; eval $V push https://github.com/acme/public.git",
            "source push.sh; gh pr create",
            "/usr/bin/gi? push https://github.com/acme/public.git",
            "/tmp/evil/git push origin",
            "~/bin/gh pr create",
            "gh api gists --input body.json",
            "gh api graphql -f query=mutation",
            "gh issue view https://ghe.example/acme/api/issues/1",
            "gh repo create new --public --source=. --push",
            "gh repo fork acme/api",
            "git push origin && gh gist create --public data.txt",
            "gh search issues retry",
            "git -C $DIR push origin",
            "git -C ~/sub push origin",
            "HOME=/tmp/evil git push origin",
            "export XDG_CONFIG_HOME=/tmp/evil; git push origin",
            "GIT_DIR=/tmp/x git push origin",
            "PATH=/tmp/bin:$PATH gh pr create",
            "GH_HOST=ghe.example gh pr view 1 --repo acme/api",
            "GH_TOKEN=other gh pr create --repo acme/api",
            "GH_REPO=acme/secret; gh pr create",
            "gh api --hostname ghe.example repos/acme/api",
            "python3 -c \"import os; os.system('git push https://github.com/acme/public.git')\"",
            "(cd ../public && git push origin)",
            "cd && git push origin",
            "cd - && git push origin",
            "popd && git push origin",
            "declare -x GIT_DIR=/tmp/evil; git push origin",
            "printf -v GIT_DIR /tmp/evil; git push origin",
            "gh repo view $REPO",
            "gh pr view $N",
            "gh pr view 1 --repo $REPO",
            "gh api repos/$OWNER/api",
            "git remote set-url origin https://github.com/acme/public.git && git push origin",
            "git config url.https://github.com/acme/public.insteadOf origin && git push origin",
            "xargs git push",
            "git -c url.x.insteadOf=y push origin",
            "git --git-dir=/tmp/x push origin",
            "git push $(cat remote.txt)",
            "gh pr create --repo `cat repo.txt`",
            "git push 'unterminated",
            'echo "$(gh pr view 1 --repo acme/public)"',
            "G=git; $G push https://github.com/acme/public.git",
        ):
            with self.assertRaises(CONTEXT.Unfollowable, msg=command):
                CONTEXT.call_targets(bash(command, "/w"))


GIT_CONFIG = """core.bare=false
remote.origin.url=git@github.com:ana/widget.git
remote.origin.fetch=+refs/heads/*:refs/remotes/origin/*
remote.upstream.url=https://github.com/acme/widget.git
remote.gitlab.url=https://gitlab.com/acme/widget.git
branch.main.remote=origin
branch.Feature.remote=upstream
"""


class Checkouts(unittest.TestCase):
    def test_remotes_are_read_from_the_git_config(self):
        config = CONTEXT.parse_git_config(GIT_CONFIG)
        self.assertEqual(CONTEXT.remote_repository(config, "origin"), "ana/widget")
        self.assertEqual(CONTEXT.remote_repository(config, "upstream"), "acme/widget")
        self.assertIsNone(CONTEXT.remote_repository(config, "gitlab"))
        with self.assertRaises(CONTEXT.Unfollowable):
            CONTEXT.remote_repository(config, "missing")

    def test_a_bare_push_follows_the_branch_then_the_defaults(self):
        config = CONTEXT.parse_git_config(GIT_CONFIG)
        self.assertEqual(CONTEXT.push_repository(config, "main"), "ana/widget")
        self.assertEqual(CONTEXT.push_repository(config, "Feature"), "acme/widget")
        self.assertEqual(CONTEXT.push_repository(config, None), "ana/widget")
        pushing = CONTEXT.parse_git_config(GIT_CONFIG + "remote.pushdefault=upstream\n")
        self.assertEqual(CONTEXT.push_repository(pushing, "main"), "acme/widget")
        pushurl = CONTEXT.parse_git_config(GIT_CONFIG + "remote.origin.pushurl=https://github.com/acme/mirror\n")
        self.assertEqual(CONTEXT.push_repository(pushurl, "main"), "acme/mirror")

    def test_gh_picks_the_default_repository_or_the_only_one(self):
        with self.assertRaises(CONTEXT.Unfollowable):
            CONTEXT.checkout_repository(CONTEXT.parse_git_config(GIT_CONFIG))
        marked = CONTEXT.parse_git_config(GIT_CONFIG + "remote.upstream.gh-resolved=base\n")
        self.assertEqual(CONTEXT.checkout_repository(marked), "acme/widget")
        named = CONTEXT.parse_git_config(GIT_CONFIG + "remote.origin.gh-resolved=acme/other\n")
        self.assertEqual(CONTEXT.checkout_repository(named), "acme/other")
        single = CONTEXT.parse_git_config("remote.origin.url=https://github.com/acme/widget\nremote.mirror.url=git@github.com:acme/widget.git\n")
        self.assertEqual(CONTEXT.checkout_repository(single), "acme/widget")
        with self.assertRaises(CONTEXT.Unfollowable):
            CONTEXT.checkout_repository(CONTEXT.parse_git_config("remote.origin.url=https://gitlab.com/acme/widget\n"))

    def test_a_real_checkout_resolves_to_one_repository_and_item(self):
        with tempfile.TemporaryDirectory() as directory:
            for arguments in (["init", "-q", "-b", "main"], ["remote", "add", "origin", "git@github.com:acme/widget.git"]):
                subprocess.run(["git", "-C", directory, *arguments], check=True, capture_output=True)
            self.assertEqual(CONTEXT.reached(CONTEXT.call_targets(bash("gh pr view 12 --comments", directory))), ("acme/widget", 12, None))
            self.assertEqual(CONTEXT.reached(CONTEXT.call_targets(bash("git push && gh pr create --fill", directory))), ("acme/widget", None, None))
            with self.assertRaises(CONTEXT.Unfollowable):
                CONTEXT.reached(CONTEXT.call_targets(bash("git push && gh pr create --repo acme/other", directory)))
            with self.assertRaises(CONTEXT.Unfollowable):
                CONTEXT.reached(CONTEXT.call_targets(bash("gh pr view 1 && gh issue view 2", directory)))

    def test_a_directory_that_is_no_checkout_is_unfollowable(self):
        with tempfile.TemporaryDirectory() as empty:
            for cwd in (empty, None, "relative/path"):
                with self.assertRaises(CONTEXT.Unfollowable):
                    CONTEXT.reached(CONTEXT.call_targets(bash("gh pr view 12", cwd)))
        self.assertEqual(CONTEXT.reached(CONTEXT.call_targets(bash("gh pr view 12 -R acme/api"))), ("acme/api", 12, None))


class Answers(unittest.TestCase):
    def test_a_pull_request_answer_names_every_author_and_editor(self):
        self.assertEqual(
            CONTEXT.answer_of(PULL_REQUEST_PAYLOAD),
            {
                "viewer": "ana",
                "repository": {"name": "acme/widget", "visibility": "public", "viewer_permission": "ADMIN", "fork_of": None},
                "pull_request": {
                    "number": 12,
                    "author": {"login": "ana", "association": "MEMBER"},
                    "locked": False,
                    "cross_repository": False,
                    "participants": [
                        {"login": "ana", "association": "MEMBER", "bot": False},
                        {"login": "renovate", "association": "NONE", "bot": True},
                        {"login": "bo", "association": "COLLABORATOR", "bot": False},
                    ],
                    "commit_authors": ["ana", None],
                    "last_editors": ["ana", "cy"],
                    "truncated": False,
                },
            },
        )

    def test_an_issue_answer_keeps_a_deleted_author_and_the_truncation(self):
        answer = CONTEXT.answer_of(ISSUE_PAYLOAD)
        self.assertEqual(answer["repository"], {"name": "acme/billing", "visibility": "private", "viewer_permission": "WRITE", "fork_of": "upstream/billing"})
        self.assertEqual(
            answer["issue"],
            {
                "number": 7,
                "author": {"login": None, "association": "NONE"},
                "locked": True,
                "participants": [{"login": None, "association": "NONE", "bot": False}, {"login": "ana", "association": "MEMBER", "bot": False}],
                "last_editors": [],
                "truncated": True,
            },
        )

    def test_any_truncated_connection_is_reported(self):
        for path in (["comments"], ["reviews"], ["commits"]):
            payload = json.loads(json.dumps(PULL_REQUEST_PAYLOAD))
            payload["data"]["repository"]["issueOrPullRequest"][path[0]]["pageInfo"]["hasNextPage"] = True
            self.assertTrue(CONTEXT.answer_of(payload)["pull_request"]["truncated"], path)
        payload = json.loads(json.dumps(PULL_REQUEST_PAYLOAD))
        payload["data"]["repository"]["issueOrPullRequest"]["reviews"]["nodes"][0]["comments"]["pageInfo"]["hasNextPage"] = True
        self.assertTrue(CONTEXT.answer_of(payload)["pull_request"]["truncated"])

    def test_a_repository_read_without_a_number_carries_no_item(self):
        payload = json.loads(json.dumps(PULL_REQUEST_PAYLOAD))
        del payload["data"]["repository"]["issueOrPullRequest"]
        self.assertEqual(set(CONTEXT.answer_of(payload)), {"viewer", "repository"})

    def test_a_number_that_names_nothing_leaves_the_repository(self):
        payload = json.loads(json.dumps(PULL_REQUEST_PAYLOAD))
        payload["data"]["repository"]["issueOrPullRequest"] = None
        payload["errors"] = [{"type": "NOT_FOUND", "path": ["repository", "issueOrPullRequest"], "message": "Could not resolve"}]
        self.assertEqual(set(CONTEXT.answer_of(payload)), {"viewer", "repository"})

    def test_a_repository_the_token_cannot_see_is_a_failure(self):
        for payload in (
            {"data": {"viewer": {"login": "ana"}, "repository": None}, "errors": [{"type": "NOT_FOUND", "path": ["repository"], "message": "Could not resolve to a Repository"}]},
            {"errors": [{"message": "Bad credentials"}]},
            {"data": {"viewer": {"login": "ana"}, "repository": {"nameWithOwner": "acme/api", "visibility": "SECRET"}}},
        ):
            with self.assertRaises(RuntimeError):
                CONTEXT.answer_of(payload)

    def test_the_query_asks_for_the_item_only_when_the_call_names_one(self):
        named = CONTEXT.variables_of("acme/api", 12)
        self.assertEqual({key: named[key] for key in ("owner", "name", "number", "withNumber")}, {"owner": "acme", "name": "api", "number": 12, "withNumber": True})
        self.assertEqual((named["withIssues"], named["withPullRequests"]), (False, False))
        self.assertEqual((CONTEXT.variables_of("acme/api", None)["number"], CONTEXT.variables_of("acme/api", None)["withNumber"]), (0, False))

    def test_the_graphql_endpoint_sits_beside_the_rest_root(self):
        self.assertEqual(CONTEXT.graphql_url("https://api.github.com"), "https://api.github.com/graphql")
        self.assertEqual(CONTEXT.graphql_url("https://ghe.example/api/v3"), "https://ghe.example/api/graphql")


def issues(scope, first=30, **fields):
    return Listing("issues", scope, first, **fields)


def pulls(scope, first=30, **fields):
    return Listing("pull_requests", scope, first, **fields)


OPEN, ALL_ISSUES, ALL_PULLS = ("OPEN",), ("OPEN", "CLOSED"), ("OPEN", "CLOSED", "MERGED")


class GhListings(unittest.TestCase):
    def listed(self, command):
        [target] = CONTEXT.bash_targets(command, "/w")
        return target.listing

    def test_a_list_gh_reads_through_graphql_is_repeated_in_its_order(self):
        for command, expected in [
            ("gh issue list", issues(PREFIX, states=OPEN)),
            ("gh issue ls --json number,title --jq '.[].title'", issues(PREFIX, states=OPEN)),
            ("gh issue list -s closed -L 5", issues(PREFIX, 5, states=("CLOSED",))),
            ("gh issue list --state=all --limit=100", issues(PREFIX, 100, states=ALL_ISSUES)),
            ("gh issue list -sall -L50", issues(PREFIX, 50, states=ALL_ISSUES)),
            ("gh issue list -s=OPEN", issues(PREFIX, states=OPEN)),
            ("gh issue list -A ana -a bo --mention cy", issues(PREFIX, states=OPEN, filters=(("createdBy", "ana"), ("assignee", "bo"), ("mentioned", "cy")))),
            ("gh pr list", pulls(PREFIX, states=OPEN)),
            ("gh pr list --state all --limit 5", pulls(PREFIX, 5, states=ALL_PULLS)),
            ("gh pr list -s closed", pulls(PREFIX, states=("CLOSED", "MERGED"))),
            ("gh pr list -s merged -B main -H feat", pulls(PREFIX, states=("MERGED",), base="main", head="feat")),
            ("gh pr list -R acme/api --json number -t '{{.}}'", pulls(PREFIX, states=OPEN)),
        ]:
            self.assertEqual(self.listed(command), expected, command)

    def test_a_list_gh_searches_or_filters_by_the_viewer_is_read_whole(self):
        for command, expected in [
            ("gh issue list --label bug", issues(WHOLE, 100, states=OPEN, labels=("bug",))),
            ("gh issue list -l bug -l p1", issues(WHOLE, 100, states=OPEN, labels=("bug", "p1"))),
            ("gh issue list -l bug,p1 -L 5", issues(WHOLE, 100, states=OPEN, labels=("bug", "p1"))),
            ("gh issue list -m v1", issues(WHOLE, 100, states=OPEN)),
            ("gh issue list --type Bug", issues(WHOLE, 100, states=OPEN)),
            ("gh issue list --app dependabot", issues(WHOLE, 100, states=OPEN)),
            ("gh issue list -A @me -a bo", issues(WHOLE, 100, states=OPEN, filters=(("assignee", "bo"),))),
            ("gh issue list -L 500", issues(WHOLE, 100, states=OPEN)),
            ("gh pr list -A ana", pulls(WHOLE, 100, states=OPEN)),
            ("gh pr list --draft", pulls(WHOLE, 100, states=OPEN)),
            ("gh pr list -d=false -B main", pulls(WHOLE, 100, states=OPEN, base="main")),
            ("gh pr list --app renovate -l deps", pulls(WHOLE, 100, states=OPEN, labels=("deps",))),
            ("gh pr list -a @me -s all", pulls(WHOLE, 100, states=ALL_PULLS)),
        ]:
            self.assertEqual(self.listed(command), expected, command)

    def test_a_list_this_provider_cannot_repeat_is_unknown(self):
        for command in (
            "gh issue list -S 'is:open author:ana'",
            "gh pr list --search review-requested:@me",
            "gh issue list --state merged",
            "gh issue list --limit many",
            "gh issue list -L 0",
            "gh issue list --limit",
            "gh issue list --unknown-flag",
            "gh issue list -m v1 -B main",
            "gh pr list --mention ana",
            "gh issue list extra",
            "gh issue list -- -x",
            "gh pr list -wd",
        ):
            self.assertEqual(self.listed(command).scope, UNKNOWN, command)

    def test_a_list_opened_in_the_browser_reads_nothing(self):
        for command in ("gh issue list --web", "gh pr list -w -s all", "gh pr list --web=true"):
            self.assertIsNone(self.listed(command), command)
        self.assertEqual(self.listed("gh pr list --web=false"), pulls(PREFIX, states=OPEN))

    def test_a_list_names_its_repository_as_other_gh_calls_do(self):
        self.assertEqual(CONTEXT.bash_targets("gh issue list -R acme/api -L 5", "/w"), [Target("slug", "acme/api", "/w", None, issues(PREFIX, 5, states=OPEN))])
        self.assertEqual(CONTEXT.bash_targets("GH_REPO=acme/api gh pr list", "/w"), [Target("slug", "acme/api", "/w", None, pulls(PREFIX, states=OPEN))])

    def test_a_command_reading_two_listings_is_unfollowable(self):
        with self.assertRaises(CONTEXT.Unfollowable):
            CONTEXT.reached(CONTEXT.call_targets(bash("gh issue list -R acme/api && gh pr list -R acme/api")))
        listed = CONTEXT.reached(CONTEXT.call_targets(bash("gh pr view 3 -R acme/api; gh pr list -R acme/api")))
        self.assertEqual(listed, ("acme/api", 3, pulls(PREFIX, states=OPEN)))


class McpListings(unittest.TestCase):
    def listed(self, tool, **arguments):
        [target] = CONTEXT.call_targets({"tool": f"mcp/github/{tool}", "arguments": {"owner": "acme", "repo": "api", **arguments}})
        return target.listing

    def test_list_issues_repeats_the_servers_graphql_read(self):
        both = ("OPEN", "CLOSED")
        for arguments, expected in [
            ({}, issues(PREFIX, states=both)),
            ({"state": "OPEN"}, issues(PREFIX, states=OPEN)),
            ({"state": "closed"}, issues(PREFIX, states=("CLOSED",))),
            ({"state": "all"}, issues(PREFIX, states=both)),
            ({"labels": ["bug", "p1"], "perPage": 100}, issues(PREFIX, 100, states=both, labels=("bug", "p1"))),
            ({"orderBy": "updated_at", "direction": "asc"}, issues(PREFIX, states=both, order=("UPDATED_AT", "ASC"))),
            ({"orderBy": "POPULARITY", "direction": "UP"}, issues(PREFIX, states=both)),
            ({"since": "2026-09-01"}, issues(PREFIX, states=both, filters=(("since", "2026-09-01T00:00:00Z"),))),
            ({"since": "2026-09-01T10:00:00+02:00"}, issues(PREFIX, states=both, filters=(("since", "2026-09-01T10:00:00+02:00"),))),
            ({"after": "Y3Vyc29yOnYy", "perPage": 10}, issues(PREFIX, 10, states=both, after="Y3Vyc29yOnYy")),
            ({"fields": ["number"], "state": None, "labels": None}, issues(PREFIX, states=both)),
            ({"field_filters": [{"field_name": "Priority", "value": "P1"}]}, issues(WHOLE, 100, states=both)),
        ]:
            self.assertEqual(self.listed("list_issues", **arguments), expected, arguments)

    def test_list_issues_the_server_would_refuse_is_unknown(self):
        for arguments in (
            {"page": 2},
            {"perPage": 101},
            {"perPage": 0},
            {"perPage": "30"},
            {"perPage": True},
            {"labels": "bug"},
            {"labels": ["bug", 3]},
            {"state": 1},
            {"since": "yesterday"},
            {"after": 7},
        ):
            self.assertEqual(self.listed("list_issues", **arguments).scope, UNKNOWN, arguments)

    def test_list_pull_requests_reads_every_rest_page_up_to_the_one_asked(self):
        for arguments, expected in [
            ({}, pulls(PREFIX, states=OPEN)),
            ({"state": "closed", "base": "main"}, pulls(PREFIX, states=("CLOSED", "MERGED"), base="main")),
            ({"state": "all", "perPage": 5}, pulls(PREFIX, 5, states=ALL_PULLS)),
            ({"sort": "updated"}, pulls(PREFIX, states=OPEN, order=("UPDATED_AT", "ASC"))),
            ({"sort": "created", "direction": "asc"}, pulls(PREFIX, states=OPEN, order=("CREATED_AT", "ASC"))),
            ({"perPage": 20, "page": 3}, pulls(PREFIX, 60, states=OPEN)),
            ({"perPage": 50, "page": 3}, pulls(WHOLE, 100, states=OPEN)),
            ({"perPage": 500}, pulls(PREFIX, 100, states=OPEN)),
            ({"sort": "popularity", "direction": "desc"}, pulls(WHOLE, 100, states=OPEN, order=("CREATED_AT", "DESC"))),
            ({"sort": "long-running"}, pulls(WHOLE, 100, states=OPEN, order=("CREATED_AT", "ASC"))),
            ({"head": "ana:feat"}, pulls(WHOLE, 100, states=OPEN, head="feat")),
            ({"head": "feat"}, pulls(WHOLE, 100, states=OPEN)),
        ]:
            self.assertEqual(self.listed("list_pull_requests", **arguments), expected, arguments)

    def test_list_pull_requests_rest_would_refuse_is_unknown(self):
        for arguments in (
            {"state": "merged"},
            {"state": "OPEN"},
            {"sort": "comments"},
            {"direction": "DESC"},
            {"page": 0},
            {"perPage": 1.5},
            {"base": ["main"]},
        ):
            self.assertEqual(self.listed("list_pull_requests", **arguments).scope, UNKNOWN, arguments)

    def test_only_the_two_listings_carry_one(self):
        self.assertIsNone(self.listed("get_file_contents", path="README.md"))
        self.assertIsNone(self.listed("search_issues", query="bug"))


def listed_node(number, login, association, typename="User", editor=None):
    return {"number": number, **written(login, association, typename, editor)}


# A recorded `issues` connection: a member's issue a collaborator edited, a
# bot's issue, an issue by an account since deleted, and one edited by an
# account since deleted.
ISSUES_PAYLOAD = {
    "data": {
        "viewer": {"login": "ana"},
        "repository": {
            "nameWithOwner": "acme/widget",
            "visibility": "PUBLIC",
            "viewerPermission": "ADMIN",
            "parent": None,
            "issues": page(
                [
                    listed_node(41, "ana", "MEMBER", editor="bo"),
                    listed_node(40, "dependabot", "NONE", "Bot"),
                    listed_node(39, None, "NONE"),
                    {**listed_node(38, "cy", "CONTRIBUTOR"), "lastEditedAt": "2026-09-02T10:00:00Z"},
                ]
            ),
        },
    }
}


class ListingAnswers(unittest.TestCase):
    def test_a_listing_answer_names_each_items_author_and_editor(self):
        self.assertEqual(
            CONTEXT.answer_of(ISSUES_PAYLOAD, issues(PREFIX, states=OPEN))["issues"],
            {
                "items": [
                    {"number": 41, "author": {"login": "ana", "association": "MEMBER"}, "bot": False, "last_editor": "bo"},
                    {"number": 40, "author": {"login": "dependabot", "association": "NONE"}, "bot": True},
                    {"number": 39, "author": {"login": None, "association": "NONE"}, "bot": False},
                    {"number": 38, "author": {"login": "cy", "association": "CONTRIBUTOR"}, "bot": False, "last_editor": None},
                ],
                "truncated": False,
            },
        )

    def test_a_further_page_truncates_only_a_listing_read_whole(self):
        payload = json.loads(json.dumps(ISSUES_PAYLOAD))
        payload["data"]["repository"]["issues"]["pageInfo"]["hasNextPage"] = True
        self.assertFalse(CONTEXT.answer_of(payload, issues(PREFIX, states=OPEN))["issues"]["truncated"])
        self.assertTrue(CONTEXT.answer_of(payload, issues(WHOLE, 100, states=OPEN))["issues"]["truncated"])

    def test_a_listing_this_provider_cannot_repeat_is_truncated_and_not_fetched(self):
        payload = json.loads(json.dumps(ISSUES_PAYLOAD))
        del payload["data"]["repository"]["issues"]
        self.assertEqual(CONTEXT.answer_of(payload, pulls(UNKNOWN, 0))["pull_requests"], {"items": [], "truncated": True})
        variables = CONTEXT.variables_of("acme/widget", None, pulls(UNKNOWN, 0))
        self.assertEqual((variables["withIssues"], variables["withPullRequests"]), (False, False))

    def test_a_response_without_the_listing_is_a_failure(self):
        with self.assertRaises(RuntimeError):
            CONTEXT.answer_of(ISSUES_PAYLOAD, pulls(PREFIX, states=OPEN))

    def test_the_query_repeats_the_listings_filters(self):
        listed = issues(PREFIX, 5, states=OPEN, labels=("bug",), order=("UPDATED_AT", "ASC"), filters=(("createdBy", "ana"),), after="Y3Vy")
        self.assertEqual(
            CONTEXT.variables_of("acme/widget", None, listed),
            {
                "owner": "acme",
                "name": "widget",
                "number": 0,
                "withNumber": False,
                "withIssues": True,
                "withPullRequests": False,
                "first": 5,
                "after": "Y3Vy",
                "issueStates": ["OPEN"],
                "pullRequestStates": None,
                "labels": ["bug"],
                "orderBy": {"field": "UPDATED_AT", "direction": "ASC"},
                "filterBy": {"createdBy": "ana"},
                "baseRefName": None,
                "headRefName": None,
            },
        )
        pulled = CONTEXT.variables_of("acme/widget", None, pulls(WHOLE, 100, states=ALL_PULLS, base="main", head="feat"))
        self.assertEqual(
            {key: pulled[key] for key in ("withPullRequests", "first", "pullRequestStates", "issueStates", "labels", "filterBy", "baseRefName", "headRefName")},
            {"withPullRequests": True, "first": 100, "pullRequestStates": list(ALL_PULLS), "issueStates": None, "labels": None, "filterBy": None, "baseRefName": "main", "headRefName": "feat"},
        )


class Envelope(unittest.TestCase):
    def run_script(self, request, env):
        return subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(request), capture_output=True, text=True, env=env)

    def test_a_call_that_reaches_nothing_answers_null_without_a_token(self):
        with tempfile.TemporaryDirectory() as empty:
            result = self.run_script(consult(bash("npm test")), {"PATH": empty})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"version": 1, "answer": None})

    def test_a_recognized_call_costs_one_graphql_query(self):
        artifact = {"tool": "mcp/github/pull_request_read", "arguments": {"owner": "acme", "repo": "widget", "pullNumber": 12}}
        with Loopback(200, PULL_REQUEST_PAYLOAD) as github:
            result = self.run_script(consult(artifact), github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(github.seen, [("/graphql", "Bearer ghp-fixture", CONTEXT.variables_of("acme/widget", 12))])
        self.assertEqual(json.loads(result.stdout), {"version": 1, "answer": CONTEXT.answer_of(PULL_REQUEST_PAYLOAD)})

    def test_a_listing_costs_the_same_one_query(self):
        artifact = {"tool": "mcp/github/list_issues", "arguments": {"owner": "acme", "repo": "widget", "state": "OPEN"}}
        with Loopback(200, ISSUES_PAYLOAD) as github:
            result = self.run_script(consult(artifact), github.env())
        self.assertEqual(result.returncode, 0, result.stderr)
        listed = issues(PREFIX, states=OPEN)
        self.assertEqual(github.seen, [("/graphql", "Bearer ghp-fixture", CONTEXT.variables_of("acme/widget", None, listed))])
        self.assertEqual(json.loads(result.stdout), {"version": 1, "answer": CONTEXT.answer_of(ISSUES_PAYLOAD, listed)})

    def test_a_github_failure_or_an_unfollowable_command_exits_nonzero(self):
        artifact = {"tool": "mcp/github/get_file_contents", "arguments": {"owner": "acme", "repo": "gone"}}
        for status, answer in ((401, {"message": "Bad credentials"}), (200, {"data": {"repository": None}, "errors": [{"message": "Could not resolve"}]})):
            with Loopback(status, answer) as github:
                result = self.run_script(consult(artifact), github.env())
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, "")
        result = self.run_script(consult(bash('bash -c "gh pr view 1"')), {"PATH": "/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")

    def test_a_missing_token_is_a_failure(self):
        artifact = {"tool": "mcp/github/get_file_contents", "arguments": {"owner": "acme", "repo": "api"}}
        with tempfile.TemporaryDirectory() as empty:
            result = self.run_script(consult(artifact), {"PATH": empty})
        self.assertEqual(result.returncode, 1)
        self.assertIn("APPA_PROVIDER_GITHUB_TOKEN", result.stderr)


if __name__ == "__main__":
    unittest.main()
