"""The databricks audience source: one consult in, one answer out.

Serves these selector templates over one workspace, through the
Databricks CLI:

  viewer                      the login's own reader
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

Every read is one `databricks <group> <command> -o json` process. The CLI
owns the workspace and the credential: its default profile, or the one
DATABRICKS_CONFIG_PROFILE names, or DATABRICKS_HOST with a token. When the
deployment sets APPA_PROVIDER_DATABRICKS_TOKEN, the binding's variable, the
source hands it to the CLI as DATABRICKS_TOKEN and nothing else changes.
The login needs to read SCIM users and groups and Genie space
permissions. A CLI failure, missing answer, or malformed response exits
nonzero: the runtime treats that as no answer and refuses the operation,
so a directory hiccup never becomes a policy decision.
"""

import json
import os
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

SOURCE_NAME = "databricks"
SERVED_TEMPLATES = ["viewer", "members", "group/<name>", "genie-space/<id>/readers"]
TOKEN_VAR = "APPA_PROVIDER_DATABRICKS_TOKEN"
CLI_TOKEN_VAR = "DATABRICKS_TOKEN"
TIMEOUT_SECONDS = 30
USER_ATTRIBUTES = "id,userName,active"
GROUP_ATTRIBUTES = "id,displayName,members"
# Users a directory listing may hold: a larger workspace cannot answer inside
# the runtime's consult budget and is refused instead.
MAX_DIRECTORY = 5000
# Members looked up singly, at most LOOKUP_WORKERS at a time, before a
# directory listing is cheaper.
DIRECT_LOOKUPS = 20
LOOKUP_WORKERS = 8
# How the CLI reports a SCIM user or group the workspace does not have, the
# one definitive absence: a bare 404 from a wrong host or gateway, a missing
# profile, and every other failure stay failures.
NOT_FOUND = re.compile(r"\b(User|Group) with id \S+ not found")


class NotFound(Exception):
    pass


def databricks_cli(environ):
    """A runner of `databricks <args> -o json` answering the parsed output."""
    env = dict(environ)
    token = (environ.get(TOKEN_VAR) or "").strip()
    if token:
        env[CLI_TOKEN_VAR] = token

    def run(*args):
        command = ["databricks", *args, "-o", "json"]
        spelled = f"`databricks {' '.join(args[:3])}`"
        try:
            completed = subprocess.run(command, capture_output=True, text=True, timeout=TIMEOUT_SECONDS, env=env, check=False)
        except FileNotFoundError:
            raise RuntimeError("the databricks CLI is not on PATH; install it where the runtime runs and log in with `databricks auth login`") from None
        except subprocess.TimeoutExpired:
            raise RuntimeError(f"{spelled} gave no answer within {TIMEOUT_SECONDS}s") from None
        if completed.returncode != 0:
            message = completed.stderr.strip() or f"exit status {completed.returncode}"
            if NOT_FOUND.search(message):
                raise NotFound(message)
            raise RuntimeError(f"{spelled} failed: {message}")
        try:
            return json.loads(completed.stdout)
        except json.JSONDecodeError:
            raise RuntimeError(f"{spelled} printed no JSON") from None

    return run


class Workspace:
    """One consult's view of the workspace: the CLI runner, every group read
    at most once, and the directory listed at most once however many groups
    the consult expands."""

    def __init__(self, run):
        self.run = run
        self.users = None
        self.groups = {}

    def directory(self):
        if self.users is None:
            self.users = {user["id"]: user for user in listed_users(self.run)}
        return self.users


def is_address(text):
    """Whether a userName is a reader address under the contract: one `@`,
    something on both sides, no whitespace, and no `:` anywhere, since a
    `:` makes a reader a qualified id."""
    if not isinstance(text, str) or text.count("@") != 1 or ":" in text:
        return False
    local, domain = text.split("@")
    return bool(local) and bool(domain) and not any(character.isspace() for character in text)


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


def listing(run, *args):
    """A list command's answer, which is every matching resource at once: the
    CLI pages through the workspace itself."""
    resources = run(*args)
    if not isinstance(resources, list):
        raise RuntimeError(f"`databricks {' '.join(args[:2])}` answered no list")
    return resources


def listed_users(run, *filter_args):
    users = listing(run, "users", "list", "--attributes", USER_ATTRIBUTES, *filter_args)
    if len(users) > MAX_DIRECTORY:
        raise RuntimeError(f"the workspace lists more than {MAX_DIRECTORY} users; map that audience from a bulk source")
    return users


def viewer_members(workspace):
    return readers_of([workspace.run("current-user", "me")])


def workspace_members(workspace):
    return readers_of(listed_users(workspace.run, "--filter", "active eq true"))


def looked_up(lookup, keys):
    """One lookup per key, the independent calls in flight together, the
    results in key order; the first failure is the consult's."""
    with ThreadPoolExecutor(max_workers=max(1, min(LOOKUP_WORKERS, len(keys)))) as workers:
        return list(workers.map(lookup, keys))


def user_by_id(workspace, user_id):
    try:
        return workspace.run("users", "get", user_id)
    except NotFound:
        raise RuntimeError(f"the directory does not report member {user_id}") from None


def users_by_id(workspace, user_ids):
    """The directory entries for exactly these ids; an id the workspace does
    not report is a failure, never a member silently dropped."""
    if len(user_ids) <= DIRECT_LOOKUPS:
        return dict(zip(user_ids, looked_up(lambda user_id: user_by_id(workspace, user_id), user_ids)))
    directory = workspace.directory()
    missing = [user_id for user_id in user_ids if user_id not in directory]
    if missing:
        raise RuntimeError(f"the directory does not report members {missing}")
    return {user_id: directory[user_id] for user_id in user_ids}


def scim_string(value):
    """A SCIM filter string literal: backslash and double quote escaped."""
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def sole_match(resources, attribute, value, what):
    """The one listed resource whose attribute is exactly the value."""
    matches = [resource for resource in resources if resource.get(attribute) == value]
    if len(matches) != 1:
        raise RuntimeError(f"{len(matches)} {what} are named {value!r}")
    return matches[0]


def group_by_name(workspace, name):
    groups = listing(workspace.run, "groups", "list", "--filter", f"displayName eq {scim_string(name)}", "--attributes", GROUP_ATTRIBUTES)
    return sole_match(groups, "displayName", name, "groups")


def group_by_id(workspace, group_id):
    if group_id not in workspace.groups:
        try:
            workspace.groups[group_id] = workspace.run("groups", "get", group_id)
        except NotFound:
            raise RuntimeError(f"the directory does not report group {group_id}") from None
    return workspace.groups[group_id]


def is_group_member(member):
    ref = str(member.get("$ref", ""))
    return ref.startswith("Groups/") or "/Groups/" in ref or member.get("type") == "Group"


def group_user_ids(workspace, groups):
    """The user ids of these groups, their nested groups expanded breadth
    first, in listing order: each level's unread groups are read together.
    A group already expanded in this walk, a cycle included, adds nothing
    twice; the users themselves are read once for the whole expansion."""
    expanded = {group["id"] for group in groups}
    user_ids = []
    frontier = list(groups)
    while frontier:
        unread = []
        for group in frontier:
            for member in group.get("members", []):
                if not is_group_member(member):
                    user_ids.append(member["value"])
                elif member["value"] not in expanded:
                    expanded.add(member["value"])
                    unread.append(member["value"])
        frontier = looked_up(lambda group_id: group_by_id(workspace, group_id), unread)
    return user_ids


def group_members(workspace, name):
    user_ids = distinct(group_user_ids(workspace, [group_by_name(workspace, name)]))
    return readers_of(users_by_id(workspace, user_ids).values())


def user_by_name(workspace, user_name):
    users = listed_users(workspace.run, "--filter", f"userName eq {scim_string(user_name)}")
    return sole_match(users, "userName", user_name, "users")


def users_by_name(workspace, user_names):
    """The directory entries for exactly these logins: a filtered listing each
    up to DIRECT_LOOKUPS, in flight together, then one directory listing; a
    login the workspace does not report is a failure, never a reader silently
    dropped."""
    if len(user_names) <= DIRECT_LOOKUPS:
        return looked_up(lambda user_name: user_by_name(workspace, user_name), user_names)
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
    groups by name. The groups are read together and expanded as one walk."""
    acl = workspace.run("permissions", "get", "genie", space_id).get("access_control_list")
    if not isinstance(acl, list):
        raise RuntimeError("the space permissions report no access control list")
    user_names = []
    group_names = []
    principals = []
    for entry in acl:
        if not entry.get("all_permissions"):
            continue
        match entry:
            case {"user_name": str() as user_name}:
                user_names.append(user_name)
            case {"group_name": str() as group_name}:
                group_names.append(group_name)
            case {"service_principal_name": str() as application_id}:
                principals.append(qualified(application_id))
            case _:
                raise RuntimeError("a permission entry names no principal")
    groups = looked_up(lambda group_name: group_by_name(workspace, group_name), group_names)
    user_ids = distinct(group_user_ids(workspace, groups))
    users = users_by_name(workspace, user_names) + list(users_by_id(workspace, user_ids).values())
    return distinct(principals + readers_of(users))


def member_principal(workspace, member):
    prefix = "databricks:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a databricks-qualified member")
    try:
        user = workspace.run("users", "get", member[len(prefix) :])
    except NotFound:
        # The workspace definitively knows no such user, who stays the
        # reader as written.
        return None
    # Without an address the member is the reader as written, in the
    # queried spelling; an inactive user stays as written too.
    reader = reader_of(user)
    return reader if reader is not None and is_address(reader) else member


def answer(run, artifact):
    if not isinstance(artifact, dict):
        raise ValueError("the artifact must be an object")
    workspace = Workspace(run)
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
    version skew between policy and script, refused before the CLI runs; the
    exit status 2 tells it apart from a provider failure.
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

    json.dump({"version": 1, "answer": answer(databricks_cli(os.environ), request.get("artifact"))}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"databricks audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
