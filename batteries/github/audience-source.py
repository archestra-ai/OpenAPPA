"""The github audience source: one consult in, one answer out.

Serves the stock `github` selector catalog over the GitHub REST API:

  viewer                  the token's own principal
  org/<org>/members       one explicitly selected organization's members
  org/<org>/team/<team>   one organization team, by slug

and the member lookup that canonicalizes one `github:<login>` reader.

Only the viewer carries a verified email: GitHub attests the token
owner's addresses through /user/emails, while a profile's public email
is whatever its owner typed, so every other member keeps the bare
`github:<login>` identity and distinct identities never merge by guess.

Credentials come from APPA_GITHUB_TOKEN (read:org and user:email
scopes). Any GitHub error or missing answer exits nonzero: the runtime
treats that as no answer and refuses the operation, so an API hiccup
never becomes a policy decision.
"""

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request


API_ROOT = "https://api.github.com"
TOKEN_VAR = "APPA_GITHUB_TOKEN"
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
                    raise NotFound(path) from error
                case 403:
                    raise Forbidden(path) from error
                case status:
                    raise RuntimeError(f"GET {path} failed: {status}") from error

    return call


def paginated(call, path):
    page = 1
    while True:
        batch = call(path, per_page=PAGE_SIZE, page=page)
        yield from batch
        if len(batch) < PAGE_SIZE:
            return
        page += 1


def bare_claims(login):
    return {"id": f"github:{login}"}


def viewer_members(call):
    claims = bare_claims(call("/user")["login"])
    try:
        addresses = call("/user/emails")
    except (NotFound, Forbidden):
        # The token cannot read its own addresses; the viewer keeps the
        # bare identity rather than a guessed email.
        addresses = []
    for address in addresses:
        if address.get("primary") and address.get("verified"):
            claims["verified_email"] = address["email"]
    return [claims]


def collection_members(call, path):
    return [bare_claims(user["login"]) for user in paginated(call, path) if user.get("type") == "User"]


def member_claims(call, member):
    prefix = "github:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a github-qualified member")
    try:
        call(f"/users/{urllib.parse.quote(member[len(prefix):])}")
    except NotFound:
        # GitHub definitively does not know this member, who keeps the
        # qualified identity.
        return None
    # The claims echo the queried spelling: GitHub canonicalizes login
    # case in its response, and claims for another id are refused.
    return {"id": member}


def answer(call, artifact):
    if not isinstance(artifact, dict):
        raise ValueError("the artifact must be an object")
    match sorted(artifact):
        case ["selector"]:
            selector = artifact["selector"]
            match selector:
                case "viewer":
                    members = viewer_members(call)
                case str():
                    match selector.split("/"):
                        case ["org", org, "members"] if org:
                            path = f"/orgs/{urllib.parse.quote(org)}/members"
                        case ["org", org, "team", team] if org and team:
                            path = f"/orgs/{urllib.parse.quote(org)}/teams/{urllib.parse.quote(team)}/members"
                        case _:
                            raise ValueError(f"{selector!r} names no collection this source serves")
                    members = collection_members(call, path)
                case _:
                    raise ValueError("the selector must be a string")
            return {"members": members}
        case ["member"]:
            return {"claims": member_claims(call, artifact["member"])}
        case _:
            raise ValueError("the artifact must carry exactly a selector or a member")


def main():
    request = json.load(sys.stdin)

    if request.get("version") != 1:
        raise ValueError("unsupported request version")
    if request.get("kind") != "audience":
        raise ValueError("unexpected consult kind")
    if request.get("name") != "github":
        raise ValueError("unexpected source name")

    token = os.environ.get(TOKEN_VAR)
    if not token:
        raise RuntimeError(f"{TOKEN_VAR} is not set")

    json.dump({"version": 1, "answer": answer(rest_api(token), request.get("artifact"))}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"github audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
