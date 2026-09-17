"""The google-workspace audience source: one consult in, one answer out.

Serves these selector templates over Google's OpenID userinfo and
Admin SDK Directory APIs:

  viewer                   the token's own reader
  full-members             every active Workspace user — no suspended,
                           no archived accounts
  group/<group-address>    one Workspace group, nested groups expanded

and the member lookup that resolves one `google-workspace:<address>`
member to its reader.

A member is an email address. A directory account's primary email is
administered by the Workspace itself, so `full-members` reports it; the
viewer's address is attested by the userinfo endpoint's own verified
flag, and without it the viewer stays the qualified
`google-workspace:<address>`. A group reports each member under the
address the group lists — the group is the source of truth for its own
membership, whatever the member's domain — and the lookup of a
`google-workspace:<address>` member answers the account's primary
address, so an alias resolves to the same reader as the account.

Credentials come from APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN: an OAuth2 access
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
TOKEN_VAR = "APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN"
SOURCE_NAME = "google-workspace"
SERVED_TEMPLATES = ["viewer", "full-members", "group/<group-address>"]
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


def viewer_members(call):
    info = call(USERINFO_URL)
    address = info.get("email")
    if not address:
        raise RuntimeError("the userinfo answer names no email")
    return [address if info.get("email_verified") else f"google-workspace:{address}"]


def full_members(call):
    users = paginated(call, f"{DIRECTORY_ROOT}/users", "users", customer="my_customer", maxResults=500)
    return [user["primaryEmail"] for user in users if not user.get("suspended") and not user.get("archived")]


def group_members(call, address):
    addresses = []
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
                    if email not in addresses:
                        addresses.append(email)
                case "GROUP":
                    nested = member["email"]
                    if nested not in visited:
                        visited.add(nested)
                        queue.append(nested)
                case other:
                    # A CUSTOMER member stands for the whole domain; an
                    # unexpandable entry must fail, never under-report.
                    raise RuntimeError(f"group {address} holds an unexpandable {other} member")
    return addresses


def member_principal(call, member):
    prefix = "google-workspace:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a google-workspace-qualified member")
    try:
        user = call(f"{DIRECTORY_ROOT}/users/{urllib.parse.quote(member[len(prefix):], safe='')}")
    except NotFound:
        # The Workspace definitively does not know this member, who
        # stays the reader as written.
        return None
    return user["primaryEmail"]


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
        print(f"google-workspace audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
