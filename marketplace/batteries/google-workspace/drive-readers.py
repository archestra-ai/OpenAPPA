"""The google-workspace.drive-readers annotator: one consult in, one answer out.

Drive gives a file the sharing of its folder, so a write reaches whoever
reads the file or folder it lands in. Before `create_file`, `copy_file`,
or `update_file` runs, this script asks the Drive API who reads that
target and answers what the write requires: trusted data that those
readers may see.

  self       only the viewer reads the target, as in the viewer's own
             My Drive root, which cannot be shared
  internal   every reader is an active Workspace user, the Workspace's
             own domain, or a group whose members all are
  public     anything else: anyone with the link, a published file,
             another domain, an outside account, or readers the token
             may not list

`copy_file` copies a file's content and `update_file` with `parentId`
moves one, both on Google's side and never through the trajectory, so
the answer also forwards the audience that already reads the source:
its content reaches the target only when the target's readers do.

Credentials come from APPA_PROVIDER_GOOGLE_WORKSPACE_TOKEN: the audience
source's token, with the drive.metadata.readonly scope added. A 403 is
an answer, readers the token may not see, so a target needs public data
and a source's content stays with the viewer. Any other API error exits
nonzero, and the runtime refuses the call without guessing.
"""

import importlib.util
import json
import os
import sys
import urllib.parse
from pathlib import Path

# The audience source's Google API client, loaded from its file beside this one.
SCRIPT = Path(__file__).resolve().with_name("audience-source.py")
SPEC = importlib.util.spec_from_file_location("audience_source", SCRIPT)
SOURCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SOURCE)

NAME = "google-workspace.drive-readers"
DRIVE_ROOT = "https://www.googleapis.com/drive/v3"
CHANGED = "google-workspace.changed"

SELF = "self"
INTERNAL = "internal"
PUBLIC = "public"
# The audience chain, narrowest first.
CHAIN = [SELF, INTERNAL, PUBLIC]
# The id Drive accepts for the viewer's My Drive root folder.
ROOT = "root"


def widest(*readers):
    return max(readers, key=CHAIN.index)


def quote(segment):
    return urllib.parse.quote(segment, safe="")


class Drive:
    """Drive and the directory as one consult sees them, each read once."""

    def __init__(self, call):
        self.call = call
        self._viewer = None
        self._users = {}
        self._permissions = {}

    def viewer(self):
        """The viewer's email and Workspace domain (`hd`), lowercased."""
        if self._viewer is None:
            info = self.call(SOURCE.USERINFO_URL)
            email = info.get("email")
            if not isinstance(email, str) or not email:
                raise RuntimeError("the userinfo answer names no email")
            self._viewer = (email.lower(), (info.get("hd") or "").lower())
        return self._viewer

    def is_internal(self, email):
        """Whether the address is the viewer or an active Workspace user."""
        email = email.lower()
        if email == self.viewer()[0]:
            return True
        if email not in self._users:
            url = f"{SOURCE.DIRECTORY_ROOT}/users/{quote(email)}"
            try:
                user = self.call(url)
                self._users[email] = not user.get("suspended") and not user.get("archived")
            except (SOURCE.NotFound, SOURCE.Forbidden):
                self._users[email] = False
        return self._users[email]

    def group_is_internal(self, address):
        """Whether every member of the group, nested groups expanded, is an
        active Workspace user. A member standing for a whole domain, or a
        group the token may not list, is not established as internal."""
        visited = {address}
        queue = [address]
        while queue:
            url = f"{SOURCE.DIRECTORY_ROOT}/groups/{quote(queue.pop(0))}/members"
            try:
                members = list(SOURCE.paginated(self.call, url, "members", maxResults=200))
            except (SOURCE.Forbidden, SOURCE.NotFound):
                return False
            for member in members:
                match member.get("type"):
                    case "USER" if member.get("status") == "SUSPENDED":
                        pass
                    case "USER" if self.is_internal(member.get("email", "")):
                        pass
                    case "GROUP":
                        nested = member.get("email", "")
                        if nested not in visited:
                            visited.add(nested)
                            queue.append(nested)
                    case _:
                        return False
        return True

    def grantee(self, permission):
        """The narrowest audience holding one permission's grantee."""
        email = permission.get("emailAddress") or ""
        match permission.get("type"):
            case "user" if email.lower() == self.viewer()[0]:
                return SELF
            case "user" if self.is_internal(email):
                return INTERNAL
            case "group" if email and self.group_is_internal(email):
                return INTERNAL
            case "domain" if self.viewer()[1] and (permission.get("domain") or "").lower() == self.viewer()[1]:
                return INTERNAL
            case _:
                return PUBLIC

    def permissions(self, file_id):
        """Every live permission on the file or folder, published views
        included; None when the token may not list them."""
        if file_id not in self._permissions:
            url = f"{DRIVE_ROOT}/files/{quote(file_id)}/permissions"
            try:
                listing = SOURCE.paginated(
                    self.call,
                    url,
                    "permissions",
                    supportsAllDrives="true",
                    includePermissionsForView="published",
                    pageSize=100,
                    fields="nextPageToken,permissions(type,emailAddress,domain,deleted)",
                )
                # A deleted account reads nothing.
                self._permissions[file_id] = [p for p in listing if not p.get("deleted")]
            except SOURCE.Forbidden:
                self._permissions[file_id] = None
        return self._permissions[file_id]

    def readers(self, file_id):
        """The narrowest audience holding everyone who reads a write's
        target: what the write requires. Readers the token may not list
        could be anyone."""
        if file_id == ROOT:
            return SELF
        permissions = self.permissions(file_id)
        if permissions is None:
            return PUBLIC
        return widest(SELF, *(self.grantee(p) for p in permissions))

    def content(self, file_id):
        """The widest audience every member of which reads a copied or
        moved file: what its content forwards. Everyone only when anyone
        may read it, the Workspace only when the Workspace's domain may;
        else the viewer, the one reader every readable file has."""
        permissions = self.permissions(file_id) or []
        domain = self.viewer()[1]
        if any(p.get("type") == "anyone" for p in permissions):
            return PUBLIC
        if domain and any(p.get("type") == "domain" and (p.get("domain") or "").lower() == domain for p in permissions):
            return INTERNAL
        return SELF

    def parent(self, file_id):
        """The folder a copy lands in when the call names none: the
        original's parent. A parent the token cannot see is one the viewer
        cannot write to, so Drive puts the copy in the viewer's root."""
        payload = self.call(f"{DRIVE_ROOT}/files/{quote(file_id)}", supportsAllDrives="true", fields="parents")
        parents = payload.get("parents") or []
        return parents[0] if parents else ROOT


def file_id(arguments, key, required):
    value = arguments.get(key)
    if value is None or value == "":
        if required:
            raise ValueError(f"the call names no {key}")
        return None
    if not isinstance(value, str):
        raise ValueError(f"{key} must be a string")
    return value


def written(readers):
    """An audience in the answer grammar: the bare `public`, else a one-entry list."""
    return PUBLIC if readers == PUBLIC else [readers]


def answer(drive, tool, arguments):
    """What a write into Drive requires, and what it forwards."""
    forwards = None
    match tool:
        case "create_file":
            target = drive.readers(file_id(arguments, "parentId", False) or ROOT)
        case "copy_file":
            source = file_id(arguments, "fileId", True)
            forwards = drive.content(source)
            target = drive.readers(file_id(arguments, "parentId", False) or drive.parent(source))
        case "update_file":
            moved = file_id(arguments, "fileId", True)
            parent = file_id(arguments, "parentId", False)
            target = drive.readers(moved)
            if parent:
                # A move adds the folder's readers to the file's own.
                target = widest(target, drive.readers(parent))
                forwards = drive.content(moved)
        case _:
            raise ValueError(f"{tool!r} is not a write this annotator labels")
    result = {
        "delta": {},
        "requires": {"trust": "trusted", "audience": {"contains": written(target)}, "history": [], "attention": []},
        "emits": [CHANGED],
    }
    if forwards is not None:
        result["forwards"] = {"audience": written(forwards)}
    return result


def call_of(consult):
    """The tool called and its arguments."""
    if not isinstance(consult, dict):
        raise ValueError("the consult must be an object")
    if consult.get("version") != 1:
        raise ValueError("unsupported request version")
    if consult.get("kind") != "annotation":
        raise ValueError("unexpected consult kind")
    if consult.get("name") != NAME:
        raise ValueError(f"unexpected annotator name {consult.get('name')!r}")
    artifact = consult.get("artifact")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    tool = args.get("name") if isinstance(args, dict) else None
    arguments = args.get("arguments") if isinstance(args, dict) else None
    if not isinstance(tool, str) or not isinstance(arguments, dict):
        raise ValueError("the call's name or arguments are missing")
    return tool.rsplit("/", 1)[-1], arguments


def check_declaration(consult):
    """The mandate must admit `self` and `internal`, the audiences this
    script answers with beside `public`. A mandate without them is a
    policy and a script of different versions, refused before any
    credential is read; the exit status 2 tells it apart from a provider
    failure."""
    declared = consult.get("declaration", {}).get("audiences")
    if not isinstance(declared, list) or not {SELF, INTERNAL} <= set(declared):
        print(f"{NAME}: the policy admits {declared!r}, this script answers with {[SELF, INTERNAL]!r}", file=sys.stderr)
        raise SystemExit(2)


def main():
    consult = json.load(sys.stdin)
    tool, arguments = call_of(consult)
    check_declaration(consult)

    token = os.environ.get(SOURCE.TOKEN_VAR)
    if not token:
        raise RuntimeError(f"{SOURCE.TOKEN_VAR} is not set")

    json.dump({"version": 1, "answer": answer(Drive(SOURCE.rest_api(token)), tool, arguments)}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"{NAME}: {error}", file=sys.stderr)
        raise SystemExit(1)
