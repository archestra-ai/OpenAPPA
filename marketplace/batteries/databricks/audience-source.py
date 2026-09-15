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


class Workspace:
    """One consult's view of the workspace: the REST call, and the directory
    read at most once however many groups the consult expands."""

    def __init__(self, call):
        self.call = call
        self.users = None

    def directory(self):
        if self.users is None:
            self.users = {user["id"]: user for user in paged_users(self.call)}
        return self.users


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


def distinct(readers):
    """The readers once each, in first-seen order."""
    return list(dict.fromkeys(readers))


def readers_of(users):
    return distinct(reader for user in users if (reader := reader_of(user)) is not None)


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


def viewer_members(workspace):
    me = workspace.call(f"{SCIM}/Me")
    return readers_of([me])


def workspace_members(workspace):
    return readers_of(paged_users(workspace.call, filter="active eq true"))


def users_by_id(workspace, user_ids):
    """The directory entries for exactly these ids; an id the workspace does
    not report is a failure, never a member silently dropped."""
    if len(user_ids) <= DIRECT_LOOKUPS:
        users = {}
        for user_id in user_ids:
            try:
                users[user_id] = workspace.call(f"{SCIM}/Users/{urllib.parse.quote(user_id, safe='')}")
            except NotFound:
                raise RuntimeError(f"the directory does not report member {user_id}") from None
        return users
    directory = workspace.directory()
    missing = [user_id for user_id in user_ids if user_id not in directory]
    if missing:
        raise RuntimeError(f"the directory does not report members {missing}")
    return {user_id: directory[user_id] for user_id in user_ids}


def scim_string(value):
    """A SCIM filter string literal: backslash and double quote escaped."""
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def sole_match(listing, attribute, value, what):
    """The one listed resource whose attribute is exactly the value."""
    matches = [resource for resource in listing.get("Resources", []) if resource.get(attribute) == value]
    if len(matches) != 1:
        raise RuntimeError(f"{len(matches)} {what} are named {value!r}")
    return matches[0]


def group_by_name(workspace, name):
    listing = workspace.call(f"{SCIM}/Groups", filter=f"displayName eq {scim_string(name)}", attributes="id,displayName,members")
    return sole_match(listing, "displayName", name, "groups")


def group_by_id(workspace, group_id):
    try:
        return workspace.call(f"{SCIM}/Groups/{urllib.parse.quote(group_id, safe='')}", attributes="id,displayName,members")
    except NotFound:
        raise RuntimeError(f"the directory does not report group {group_id}") from None


def is_group_member(member):
    return "/Groups/" in str(member.get("$ref", "")) or member.get("type") == "Group"


def group_user_ids(workspace, group, depth=0):
    """The user ids of one group, its nested groups expanded, in listing order;
    the users themselves are read once for the whole expansion."""
    if depth > MAX_GROUP_DEPTH:
        raise RuntimeError(f"group {group.get('displayName')!r} nests deeper than {MAX_GROUP_DEPTH} groups")
    user_ids = []
    for member in group.get("members", []):
        if is_group_member(member):
            user_ids.extend(group_user_ids(workspace, group_by_id(workspace, member["value"]), depth + 1))
        else:
            user_ids.append(member["value"])
    return user_ids


def group_members(workspace, name):
    user_ids = distinct(group_user_ids(workspace, group_by_name(workspace, name)))
    return readers_of(users_by_id(workspace, user_ids).values())


def users_by_name(workspace, user_names):
    """The directory entries for exactly these logins: a filtered listing each
    up to DIRECT_LOOKUPS, then one directory pass; a login the workspace does
    not report is a failure, never a reader silently dropped."""
    if len(user_names) <= DIRECT_LOOKUPS:
        users = []
        for user_name in user_names:
            listing = workspace.call(f"{SCIM}/Users", filter=f"userName eq {scim_string(user_name)}", attributes="id,userName,active")
            users.append(sole_match(listing, "userName", user_name, "users"))
        return users
    by_name = {}
    for user in workspace.directory().values():
        by_name.setdefault(user.get("userName"), []).append(user)
    users = []
    for user_name in user_names:
        match by_name.get(user_name, []):
            case [user]:
                users.append(user)
            case found:
                raise RuntimeError(f"{len(found)} users are named {user_name!r}")
    return users


def genie_space_readers(workspace, space_id):
    """Everyone holding any permission level on the space, as the Permissions
    API lists them: service principals by application id, users by login,
    groups by name."""
    acl = workspace.call(f"/api/2.0/permissions/genie/{urllib.parse.quote(space_id, safe='')}").get("access_control_list")
    if not isinstance(acl, list):
        raise RuntimeError("the space permissions report no access control list")
    user_names = []
    user_ids = []
    principals = []
    for entry in acl:
        if not entry.get("all_permissions"):
            continue
        match entry:
            case {"user_name": str() as user_name}:
                user_names.append(user_name)
            case {"group_name": str() as group_name}:
                user_ids.extend(group_user_ids(workspace, group_by_name(workspace, group_name)))
            case {"service_principal_name": str() as application_id}:
                principals.append(qualified(application_id))
            case _:
                raise RuntimeError("a permission entry names no principal")
    users = users_by_name(workspace, user_names) + list(users_by_id(workspace, distinct(user_ids)).values())
    return distinct(principals + readers_of(users))


def member_principal(workspace, member):
    prefix = "databricks:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a databricks-qualified member")
    try:
        user = workspace.call(f"{SCIM}/Users/{urllib.parse.quote(member[len(prefix) :], safe='')}")
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
    workspace = Workspace(call)
    match sorted(artifact):
        case ["selector"]:
            selector = artifact["selector"]
            match selector:
                case "viewer":
                    members = viewer_members(workspace)
                case "members":
                    members = workspace_members(workspace)
                case str() if selector.startswith("group/") and len(selector) > len("group/"):
                    members = group_members(workspace, selector[len("group/") :])
                case str() if selector.startswith("genie-space/") and selector.endswith("/readers"):
                    space_id = selector[len("genie-space/") : -len("/readers")]
                    if not space_id or "/" in space_id:
                        raise ValueError(f"{selector!r} names no Genie space")
                    members = genie_space_readers(workspace, space_id)
                case _:
                    raise ValueError(f"{selector!r} names no collection this source serves")
            return {"members": members}
        case ["member"]:
            return {"principal": member_principal(workspace, artifact["member"])}
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
