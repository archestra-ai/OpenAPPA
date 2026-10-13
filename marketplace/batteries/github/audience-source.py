"""The github audience source: one consult in, one answer out.

Serves these selector templates over the GitHub REST API:

  viewer                  the token's own reader
  org/<org>/members       one explicitly selected organization's members
  org/<org>/team/<team>   one organization team, by slug
  repo/<owner>/<repo>/collaborators
                          one repository's collaborators: direct and
                          outside collaborators, and for an organization
                          repository the members who reach it through a
                          team or the organization's base permission

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

Credentials come from APPA_PROVIDER_GITHUB_TOKEN, else the GitHub CLI's
login (see github_token.py). The token needs read:org and user:email;
listing a repository's collaborators needs push access to it, which the
repo scope grants for repositories the token's owner may push to. The
API root is GITHUB_API_URL when set (a GitHub Enterprise Server's
/api/v3), else api.github.com. Any GitHub error or missing answer exits
nonzero: the runtime treats that as no answer and refuses the
operation, so an API hiccup never becomes a policy decision.
"""

import concurrent.futures
import os
import sys
import urllib.parse

# The sibling modules are found beside this file however the file is loaded:
# run by the runtime from its own directory, or imported by path from another.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from github_audience import Forbidden, NotFound, collection_path, people, serve  # noqa: E402
from github_token import resolve_token  # noqa: E402


SOURCE_NAME = "github"
SERVED_TEMPLATES = [
    "viewer",
    "org/<org>/members",
    "org/<org>/team/<team>",
    "repo/<owner>/<repo>/collaborators",
]
# Accounts a collection may hold: each costs one profile read, and a larger
# roster cannot answer inside the runtime's consult budget, so it is refused
# instead of timing out halfway.
MAX_MEMBERS = 1000


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
    logins = []
    for user in people(call, path):
        logins.append(user["login"])
        if len(logins) > MAX_MEMBERS:
            raise RuntimeError(f"{path} lists more than {MAX_MEMBERS} accounts; map that audience from a bulk source")
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
                    members = collection_members(call, collection_path(selector))
                case _:
                    raise ValueError("the selector must be a string")
            return {"members": members}
        case ["member"]:
            return {"principal": member_principal(call, artifact["member"])}
        case _:
            raise ValueError("the artifact must carry exactly a selector or a member")


if __name__ == "__main__":
    serve(SOURCE_NAME, SERVED_TEMPLATES, answer, resolve_token)
