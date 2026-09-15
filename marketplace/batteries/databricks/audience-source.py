"""The databricks audience source: one consult in, one answer out.

Serves these selector templates over one workspace's REST API:

  viewer                      the token's own reader
  members                     every active user of the workspace
  group/<name>                one workspace group's active users, nested
                              groups included
  genie-space/<id>/readers    everyone holding any permission on one
                              Genie space: its users, its groups' users,
                              and its service principals

and the member lookup that resolves one `databricks:<id>` member to its
reader.

A reader is an active user's SCIM `userName` where that is an address:
Databricks authenticates every login against it, through the identity
provider's assertion or the address's own password flow, which is the
verification the audience contract asks of a provider. Any other user is
`databricks:<id>`, a service principal `databricks:<application id>`, and
neither merges with another provider's reader. An inactive user reads
nothing and is left out.

The workspace and token come from databricks_token.py: the
APPA_PROVIDER_DATABRICKS_* variables, else the Databricks CLI's login. The
token needs to read SCIM users and groups and Genie space permissions.
Any API error, missing answer, or malformed response exits nonzero: the
runtime treats that as no answer and refuses the operation, so a
directory hiccup never becomes a policy decision.
"""

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

# The sibling module is found beside this file however the file is loaded:
# run by the runtime from its own directory, or imported by path from another.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import databricks_token  # noqa: E402

SOURCE_NAME = "databricks"
SERVED_TEMPLATES = ["viewer", "members", "group/<name>", "genie-space/<id>/readers"]
SCIM = "/api/2.0/preview/scim/v2"
TIMEOUT_SECONDS = 30
PAGE_SIZE = 100
# Users a directory pass may hold: a larger workspace cannot answer inside
# the runtime's consult budget and is refused instead of timing out halfway.
MAX_DIRECTORY = 5000
# Members looked up one by one before a directory pass is cheaper.
DIRECT_LOOKUPS = 20
# Groups nested inside groups, before the expansion is refused as a cycle.
MAX_GROUP_DEPTH = 5


class NotFound(Exception):
    pass


def rest_api(host, token):
    def call(path, **params):
        query = f"?{urllib.parse.urlencode(params)}" if params else ""
        request = urllib.request.Request(
            f"{host}{path}{query}",
            headers={"Authorization": f"Bearer {token}", "Accept": "application/json"},
        )
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                raise NotFound(path) from error
            raise RuntimeError(f"GET {path} failed: HTTP {error.code}") from error

    return call


def is_address(text):
    """Whether a userName is a reader address under the contract: one `@`,
    something on both sides, no whitespace, and no `:` before the `@`."""
    if not isinstance(text, str) or text.count("@") != 1 or any(character.isspace() for character in text):
        return False
    local, domain = text.split("@")
    return bool(local) and bool(domain) and ":" not in local


def qualified(user_id):
    return f"databricks:{user_id}"


def reader_of(user):
    """The reader an active user is, `None` for an inactive one."""
    if user.get("active") is not True:
        return None
    user_name = user.get("userName")
    return user_name if is_address(user_name) else qualified(user["id"])


def readers_of(users):
    readers = []
    for user in users:
        reader = reader_of(user)
        if reader is not None and reader not in readers:
            readers.append(reader)
    return readers


def paged_users(call, **params):
    """Every user of one SCIM listing, page by page, refused past the bound."""
    start = 1
    while True:
        page = call(f"{SCIM}/Users", attributes="id,userName,active", count=PAGE_SIZE, startIndex=start, **params)
        total = page.get("totalResults")
        if not isinstance(total, int):
            raise RuntimeError("the user listing reports no total")
        if total > MAX_DIRECTORY:
            raise RuntimeError(f"the workspace lists more than {MAX_DIRECTORY} users; map that audience from a bulk source")
        resources = page.get("Resources", [])
        if not resources and start <= total:
            raise RuntimeError("the user listing returned an empty page before its total")
        yield from resources
        start += len(resources)
        if start > total or not resources:
            return


def viewer_members(call):
    me = call(f"{SCIM}/Me")
    return readers_of([me])


def workspace_members(call):
    return readers_of(paged_users(call, filter="active eq true"))


def users_by_id(call, user_ids):
    """The directory entries for exactly these ids; an id the workspace does
    not report is a failure, never a member silently dropped."""
    if len(user_ids) <= DIRECT_LOOKUPS:
        users = {}
        for user_id in user_ids:
            try:
                users[user_id] = call(f"{SCIM}/Users/{urllib.parse.quote(user_id, safe='')}")
            except NotFound:
                raise RuntimeError(f"the directory does not report member {user_id}") from None
        return users
    directory = {user["id"]: user for user in paged_users(call)}
    missing = [user_id for user_id in user_ids if user_id not in directory]
    if missing:
        raise RuntimeError(f"the directory does not report members {missing}")
    return {user_id: directory[user_id] for user_id in user_ids}


def group_by_name(call, name):
    listing = call(f"{SCIM}/Groups", filter=f'displayName eq "{name}"', attributes="id,displayName,members")
    matches = [group for group in listing.get("Resources", []) if group.get("displayName") == name]
    if len(matches) != 1:
        raise RuntimeError(f"{len(matches)} groups are named {name!r}")
    return matches[0]


def group_by_id(call, group_id):
    try:
        return call(f"{SCIM}/Groups/{urllib.parse.quote(group_id, safe='')}", attributes="id,displayName,members")
    except NotFound:
        raise RuntimeError(f"the directory does not report group {group_id}") from None


def is_group_member(member):
    return "/Groups/" in str(member.get("$ref", "")) or member.get("type") == "Group"


def group_users(call, group, depth=0):
    """The users of one group, its nested groups expanded, in listing order."""
    if depth > MAX_GROUP_DEPTH:
        raise RuntimeError(f"group {group.get('displayName')!r} nests deeper than {MAX_GROUP_DEPTH} groups")
    members = group.get("members", [])
    user_ids = [member["value"] for member in members if not is_group_member(member)]
    users = list(users_by_id(call, user_ids).values())
    for member in members:
        if is_group_member(member):
            users.extend(group_users(call, group_by_id(call, member["value"]), depth + 1))
    return users


def group_members(call, name):
    return readers_of(group_users(call, group_by_name(call, name)))


def user_by_name(call, user_name):
    listing = call(f"{SCIM}/Users", filter=f'userName eq "{user_name}"', attributes="id,userName,active")
    matches = [user for user in listing.get("Resources", []) if user.get("userName") == user_name]
    if len(matches) != 1:
        raise RuntimeError(f"{len(matches)} users are named {user_name!r}")
    return matches[0]


def genie_space_readers(call, space_id):
    """Everyone holding any permission level on the space, as the Permissions
    API lists them: users by login, groups by name, service principals by
    application id."""
    acl = call(f"/api/2.0/permissions/genie/{urllib.parse.quote(space_id, safe='')}").get("access_control_list")
    if not isinstance(acl, list):
        raise RuntimeError("the space permissions report no access control list")
    users = []
    readers = []
    for entry in acl:
        if not entry.get("all_permissions"):
            continue
        match entry:
            case {"user_name": str() as user_name}:
                users.append(user_by_name(call, user_name))
            case {"group_name": str() as group_name}:
                users.extend(group_users(call, group_by_name(call, group_name)))
            case {"service_principal_name": str() as application_id}:
                readers.append(qualified(application_id))
            case _:
                raise RuntimeError("a permission entry names no principal")
    for reader in readers_of(users):
        if reader not in readers:
            readers.append(reader)
    return readers


def member_principal(call, member):
    prefix = "databricks:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a databricks-qualified member")
    try:
        user = call(f"{SCIM}/Users/{urllib.parse.quote(member[len(prefix) :], safe='')}")
    except NotFound:
        # The workspace definitively knows no such user, who stays the
        # reader as written.
        return None
    # Without an address the member is the reader as written, in the
    # queried spelling; an inactive user stays as written too.
    reader = reader_of(user)
    return reader if reader is not None and reader != qualified(user["id"]) else member


def answer(call, artifact):
    if not isinstance(artifact, dict):
        raise ValueError("the artifact must be an object")
    match sorted(artifact):
        case ["selector"]:
            selector = artifact["selector"]
            match selector:
                case "viewer":
                    members = viewer_members(call)
                case "members":
                    members = workspace_members(call)
                case str() if selector.startswith("group/") and len(selector) > len("group/"):
                    members = group_members(call, selector[len("group/") :])
                case str() if selector.startswith("genie-space/") and selector.endswith("/readers"):
                    space_id = selector[len("genie-space/") : -len("/readers")]
                    if not space_id or "/" in space_id:
                        raise ValueError(f"{selector!r} names no Genie space")
                    members = genie_space_readers(call, space_id)
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

    host, token = databricks_token.resolve()

    json.dump({"version": 1, "answer": answer(rest_api(host, token), request.get("artifact"))}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"databricks audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
