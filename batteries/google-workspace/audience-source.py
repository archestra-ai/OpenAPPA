"""The google-workspace audience source: one consult in, one answer out.

Serves the stock `google-workspace` selector catalog over Google's
OpenID userinfo and Admin SDK Directory APIs:

  viewer                   the token's own principal
  full-members             every active Workspace user — no suspended,
                           no archived accounts
  group/<group-address>    one Workspace group, nested groups expanded

and the member lookup that canonicalizes one
`google-workspace:<address>` reader.

A Workspace account's primary email is administered and attested by the
Workspace itself, so directory members carry it as their verified
email; the viewer's email is attested by the userinfo endpoint's own
verified flag. Belonging to a group proves membership, not identity: a
group member outside the directory belongs to the group like any other
— the group is the source of truth for its own membership — but keeps
its qualified identity, because the Workspace administers no account
for that address and attests nothing about it.

Credentials come from OPENAPPA_GOOGLE_WORKSPACE_TOKEN: an OAuth2 access
token with the admin.directory.user.readonly and
admin.directory.group.member.readonly scopes plus openid email. Any
API error or missing answer exits nonzero: the runtime treats that as
no answer and refuses the operation, so a directory hiccup never
becomes a policy decision.
"""

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request


USERINFO_URL = "https://openidconnect.googleapis.com/v1/userinfo"
DIRECTORY_ROOT = "https://admin.googleapis.com/admin/directory/v1"
TOKEN_VAR = "OPENAPPA_GOOGLE_WORKSPACE_TOKEN"
TIMEOUT_SECONDS = 30


class NotFound(Exception):
    """Google answered 404: the path names nothing the token can see."""


def rest_api(token):
    def call(url, **params):
        query = f"?{urllib.parse.urlencode(params)}" if params else ""
        request = urllib.request.Request(
            f"{url}{query}",
            headers={"Authorization": f"Bearer {token}"},
        )
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                raise NotFound(url) from error
            raise RuntimeError(f"GET {url} failed: {error.code}") from error

    return call


def paginated(call, url, key, **params):
    token = None
    while True:
        page = call(url, **params, **({"pageToken": token} if token else {}))
        yield from page.get(key, [])
        token = page.get("nextPageToken")
        if not token:
            return


def claims_of(address):
    return {"id": f"google-workspace:{address}", "verified_email": address}


def viewer_members(call):
    info = call(USERINFO_URL)
    address = info.get("email")
    if not address:
        raise RuntimeError("the userinfo answer names no email")
    claims = {"id": f"google-workspace:{address}"}
    if info.get("email_verified"):
        claims["verified_email"] = address
    return [claims]


def directory_users(call):
    """Every account the Workspace itself administers, live and reachable."""
    return [
        user
        for user in paginated(call, f"{DIRECTORY_ROOT}/users", "users", customer="my_customer", maxResults=500)
        if not user.get("suspended") and not user.get("archived")
    ]


def full_members(call):
    return [claims_of(user["primaryEmail"]) for user in directory_users(call)]


def group_members(call, address):
    # Membership proves that someone belongs to the group. Only a directory
    # account proves that this Workspace administers their email identity, and
    # the API's own member `type` does not draw that line: its EXTERNAL value
    # is documented as unused, so an outside auditor arrives as an ordinary
    # USER. One directory pass answers the question the type cannot.
    administered = {user["primaryEmail"] for user in directory_users(call)}
    members = []
    visited = {address}
    queue = [address]
    while queue:
        group_url = f"{DIRECTORY_ROOT}/groups/{urllib.parse.quote(queue.pop(0), safe='')}/members"
        for member in paginated(call, group_url, "members", maxResults=200):
            match member.get("type"):
                case "USER" | "EXTERNAL" if member.get("status") == "SUSPENDED":
                    pass
                case "USER" | "EXTERNAL":
                    email = member["email"]
                    # A member the directory does not administer belongs to the
                    # group like any other, but the Workspace attests nothing
                    # about their address: they keep their qualified identity
                    # and merge with no other provider's reader.
                    members.append(
                        claims_of(email) if email in administered else {"id": f"google-workspace:{email}"}
                    )
                case "GROUP":
                    nested = member["email"]
                    if nested not in visited:
                        visited.add(nested)
                        queue.append(nested)
                case other:
                    # A CUSTOMER member stands for the whole domain; an
                    # unexpandable entry must fail, never under-report.
                    raise RuntimeError(f"group {address} holds an unexpandable {other} member")
    unique = []
    for claims in members:
        if claims not in unique:
            unique.append(claims)
    return unique


def member_claims(call, member):
    prefix = "google-workspace:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a google-workspace-qualified member")
    try:
        user = call(f"{DIRECTORY_ROOT}/users/{urllib.parse.quote(member[len(prefix):], safe='')}")
    except NotFound:
        # The Workspace definitively does not know this member, who
        # keeps the qualified identity.
        return None
    # The claims echo the queried spelling — claims for another id are
    # refused — and carry the account's administered primary address.
    return {"id": member, "verified_email": user["primaryEmail"]}


def answer(call, artifact):
    if not isinstance(artifact, dict):
        raise ValueError("the artifact must be an object")
    match sorted(artifact):
        case ["selector"]:
            selector = artifact["selector"]
            match selector:
                case "viewer":
                    members = viewer_members(call)
                case "full-members":
                    members = full_members(call)
                case str() if selector.startswith("group/") and len(selector) > len("group/"):
                    members = group_members(call, selector[len("group/") :])
                case _:
                    raise ValueError(f"{selector!r} names no collection this source serves")
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
    if request.get("name") != "google-workspace":
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
        print(f"google-workspace audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
