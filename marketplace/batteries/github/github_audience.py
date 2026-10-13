"""What the github audience sources share: the GitHub REST reads, the
collection each selector names, and the consult envelope.

`audience-source.py` serves a single-user deployment, whose token is a
person's; `audience-source-multi-user.py` serves a multi-user one, whose
token is a GitHub App installation's. Each declares the templates it
serves and answers a consult with its own reader spelling.
"""

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from github_token import api_root  # noqa: E402


API_ROOT = api_root(os.environ)
TIMEOUT_SECONDS = 30
PAGE_SIZE = 100


class NotFound(Exception):
    """GitHub answered 404: the path names nothing the token can see."""


class Forbidden(Exception):
    """GitHub answered 403: the token lacks the scope for this path."""


def rest_api(token):
    def call(path, **params):
        query = f"?{urllib.parse.urlencode(params)}" if params else ""
        request = urllib.request.Request(
            f"{API_ROOT}{path}{query}",
            headers={
                "Authorization": f"Bearer {token}",
                "Accept": "application/vnd.github+json",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            match error.code:
                case 404:
                    raise NotFound(f"GET {path} failed: 404") from error
                case 403:
                    raise Forbidden(f"GET {path} failed: 403") from error
                case status:
                    raise RuntimeError(f"GET {path} failed: {status}") from error

    return call


def paginated(call, path, max_pages=None):
    """Every page of a listing; past `max_pages` full pages the listing is
    refused, never cut short."""
    page = 1
    while True:
        batch = call(path, per_page=PAGE_SIZE, page=page)
        if not isinstance(batch, list):
            raise RuntimeError(f"{path} page {page} is not a JSON array")
        yield from batch
        if len(batch) < PAGE_SIZE:
            return
        if page == max_pages:
            raise RuntimeError(f"{path} lists {max_pages * PAGE_SIZE} or more accounts; map that audience from a bulk source")
        page += 1


def collection_path(selector):
    """The REST listing a collection selector names; any other spelling,
    `viewer` included, is refused."""
    def segment(value):
        return urllib.parse.quote(value, safe="")

    match selector.split("/"):
        case ["org", org, "members"] if org:
            return f"/orgs/{segment(org)}/members"
        case ["org", org, "team", team] if org and team:
            return f"/orgs/{segment(org)}/teams/{segment(team)}/members"
        case ["repo", owner, repo, "collaborators"] if owner and repo:
            return f"/repos/{segment(owner)}/{segment(repo)}/collaborators"
        case _:
            raise ValueError(f"{selector!r} names no collection this source serves")


def people(call, path, max_pages=None):
    """The accounts a collection lists, its bots left out: a GitHub App
    acts for the people who installed it and reads as none of its own. An
    account of any other or no type is refused, never skipped."""
    for account in paginated(call, path, max_pages):
        match account:
            case {"type": "User"}:
                yield account
            case {"type": "Bot"}:
                continue
            case _:
                raise RuntimeError(f"{path} lists an account of unrecognized type: {account!r}")


def check_declaration(request, source_name, served):
    """The policy's declared templates against the ones the script serves.

    The binding beside the script declares them to the policy, and the
    runtime sends that declaration with every consult. A mismatch is a
    version skew between policy and script, refused before any credential
    is read; the exit status 2 tells it apart from a provider failure.
    """
    declared = request.get("declaration", {}).get("templates")
    if declared != served:
        print(
            f"{source_name} audience source: the policy declares {declared!r}, this script serves {served!r}",
            file=sys.stderr,
        )
        raise SystemExit(2)


def serve(source_name, served, answer, token):
    """Answer one consult from stdin on stdout: `answer(call, artifact)`
    over the REST reads `token()` authorizes. Any failure exits nonzero:
    the runtime treats that as no answer and refuses the operation, so an
    API hiccup never becomes a policy decision."""
    try:
        request = json.load(sys.stdin)
        if request.get("version") != 1:
            raise ValueError("unsupported request version")
        if request.get("kind") != "audience":
            raise ValueError("unexpected consult kind")
        if request.get("name") != source_name:
            raise ValueError("unexpected source name")
        check_declaration(request, source_name, served)

        json.dump({"version": 1, "answer": answer(rest_api(token()), request.get("artifact"))}, sys.stdout)
        sys.stdout.write("\n")
    except Exception as error:
        print(f"{source_name} audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
