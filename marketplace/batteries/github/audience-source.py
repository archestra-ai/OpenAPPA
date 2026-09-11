"""The github audience source: one consult in, one answer out.

Serves these selector templates over the GitHub REST API:

  viewer                  the token's own reader
  org/<org>/members       one explicitly selected organization's members
  org/<org>/team/<team>   one organization team, by slug

and the member lookup that resolves one `github:<login>` member to its
reader.

A member is the email address GitHub verifies for the account, else the
qualified `github:<login>`. The viewer's address is the token owner's
primary verified address from /user/emails. Any other member's address
is the email published on its profile, read from /users/{login}:
GitHub lets an account publish only one of its verified addresses
there, so a published profile email is attested. An account that
publishes none stays `github:<login>` and merges with no other
provider's reader.

Credentials come from APPA_PROVIDER_GITHUB_TOKEN (read:org and user:email
scopes). Any GitHub error or missing answer exits nonzero: the runtime
treats that as no answer and refuses the operation, so an API hiccup
never becomes a policy decision.
"""

import concurrent.futures
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request


API_ROOT = "https://api.github.com"
TOKEN_VAR = "APPA_PROVIDER_GITHUB_TOKEN"
SOURCE_NAME = "github"
SERVED_TEMPLATES = ["viewer", "org/<org>/members", "org/<org>/team/<team>"]
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


def qualified(login):
    return f"github:{login}"


def viewer_reader(call):
    login = call("/user")["login"]
    try:
        addresses = call("/user/emails")
    except (NotFound, Forbidden):
        # The token cannot read its own addresses; the viewer keeps the
        # qualified id rather than a guessed email.
        addresses = []
    for address in addresses:
        if address.get("primary") and address.get("verified"):
            return address["email"]
    return qualified(login)


def profile_reader(call, login):
    """The address the account publishes on its profile — GitHub admits
    only a verified one there — else the qualified id in the caller's
    spelling, so a lookup answers the member as the selector spelled it
    whatever case GitHub canonicalizes the login to."""
    match call(f"/users/{urllib.parse.quote(login, safe='')}").get("email"):
        case str() as email if email:
            return email
        case _:
            return qualified(login)


# Profile reads a collection answer waits on at once: enough to keep a
# large organization inside one consult budget, few enough for the
# secondary rate limit GitHub applies to concurrent requests.
PROFILE_READS = 8


def collection_members(call, path):
    logins = [user["login"] for user in paginated(call, path) if user.get("type") == "User"]
    with concurrent.futures.ThreadPoolExecutor(max_workers=PROFILE_READS) as pool:
        return list(pool.map(lambda login: profile_reader(call, login), logins))


def member_principal(call, member):
    prefix = "github:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a github-qualified member")
    try:
        return profile_reader(call, member[len(prefix) :])
    except NotFound:
        # GitHub definitively does not know this member, who stays the
        # reader as written.
        return None


def answer(call, artifact):
    if not isinstance(artifact, dict):
        raise ValueError("the artifact must be an object")
    match sorted(artifact):
        case ["selector"]:
            selector = artifact["selector"]
            match selector:
                case "viewer":
                    members = [viewer_reader(call)]
                case str():
                    match selector.split("/"):
                        case ["org", org, "members"] if org:
                            path = f"/orgs/{urllib.parse.quote(org, safe='')}/members"
                        case ["org", org, "team", team] if org and team:
                            org = urllib.parse.quote(org, safe="")
                            path = f"/orgs/{org}/teams/{urllib.parse.quote(team, safe='')}/members"
                        case _:
                            raise ValueError(f"{selector!r} names no collection this source serves")
                    members = collection_members(call, path)
                case _:
                    raise ValueError("the selector must be a string")
            return {"members": members}
        case ["member"]:
            return {"principal": member_principal(call, artifact["member"])}
        case _:
            raise ValueError("the artifact must carry exactly a selector or a member")


def check_declaration(request):
    """The policy's declared templates against the ones this script serves.

    The binding beside this script declares them to the policy, and the
    runtime sends that declaration with every consult. A mismatch is a
    version skew between policy and script, refused before any credential
    is read; the exit status 2 tells it apart from a provider failure.
    """
    declared = request.get("declaration", {}).get("templates")
    if declared != SERVED_TEMPLATES:
        print(
            f"{SOURCE_NAME} audience source: the policy declares {declared!r}, this script serves {SERVED_TEMPLATES!r}",
            file=sys.stderr,
        )
        raise SystemExit(2)


def main():
    request = json.load(sys.stdin)

    if request.get("version") != 1:
        raise ValueError("unsupported request version")
    if request.get("kind") != "audience":
        raise ValueError("unexpected consult kind")
    if request.get("name") != SOURCE_NAME:
        raise ValueError("unexpected source name")
    check_declaration(request)

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
