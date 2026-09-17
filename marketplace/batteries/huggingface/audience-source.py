"""The huggingface audience source: one consult in, one answer out.

Serves these selector templates over the Hub API:

  viewer                  the token's own reader
  org/<org>/resource-group/<group>/members
                          one resource group of one organization, by
                          its id or its name: the members who read the
                          private repositories the group holds, those
                          with the `no_access` role excluded

and the member lookup that resolves one `huggingface:<name>` member to
its reader.

The viewer is the email address the Hub verified for the account, else
the qualified `huggingface:<name>`. Every other member stays
`huggingface:<name>`: the Hub attests no address for another account to
a read token, and a qualified id merges with no other provider's
reader. The member lookup answers the viewer's own name as the viewer,
so the viewer seated in a group and the viewer read as `self` are one
reader.

There is no organization-wide members selector: members can hold the
`no_access` role, which reads no private repository, and the members
endpoint reports no role to a read token. A resource group's listing
comes from `/api/organizations/<org>/resource-groups`, which lists the
groups the token has access to with their users and roles; a group the
token cannot see is refused, never guessed.

Credentials come from APPA_PROVIDER_HUGGINGFACE_TOKEN, else the Hugging
Face CLI's stored login (see hf_token.py); the Hub root is HF_ENDPOINT
when set. Any Hub error or missing answer exits nonzero: the runtime
treats that as no answer and refuses the operation, so an API hiccup
never becomes a policy decision.
"""

import json
import sys
import urllib.parse
from pathlib import Path

# The sibling module is found beside this file however the file is loaded:
# run by the runtime from its own directory, or imported by path from another.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from hf_token import Forbidden, NotFound, hub_api, resolve_token  # noqa: E402, F401


SOURCE_NAME = "huggingface"
SERVED_TEMPLATES = ["viewer", "org/<org>/resource-group/<group>/members"]
# Accounts a collection may hold: a larger roster cannot answer inside the
# runtime's consult budget, so it is refused instead of timing out halfway.
MAX_MEMBERS = 1000
NO_ACCESS = "no_access"


def qualified(name):
    return f"huggingface:{name}"


def whoami(call):
    account = call("/api/whoami-v2")
    name = account.get("name") if isinstance(account, dict) else None
    if not isinstance(name, str) or not name:
        raise RuntimeError("whoami-v2 reports no account name")
    return account


def viewer_reader(account):
    """The verified address the Hub reports for the token's account, else its qualified name."""
    email = account.get("email")
    if account.get("emailVerified") is True and isinstance(email, str) and email:
        return email
    return qualified(account["name"])


def group_members(call, org, group):
    """The readers of one resource group: its users with any role but
    `no_access`. The group is found by id or by name among the groups the
    token can see."""
    listing = call(f"/api/organizations/{urllib.parse.quote(org, safe='')}/resource-groups")
    if not isinstance(listing, list):
        raise RuntimeError(f"the resource groups of {org} are not a list")
    for entry in listing:
        if isinstance(entry, dict) and group in (entry.get("id"), entry.get("name")):
            users = entry.get("users")
            if not isinstance(users, list):
                raise RuntimeError(f"resource group {group} of {org} lists no users")
            if len(users) > MAX_MEMBERS:
                raise RuntimeError(f"resource group {group} of {org} lists more than {MAX_MEMBERS} accounts")
            members = []
            for user in users:
                match user:
                    case {"name": str() as name, "role": str() as role} if name and role:
                        if role != NO_ACCESS:
                            members.append(qualified(name))
                    case _:
                        raise RuntimeError(f"resource group {group} of {org} lists a user without a name or a role")
            return members
    raise NotFound(f"resource group {group} of {org}")


def member_principal(call, member):
    prefix = "huggingface:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a huggingface-qualified member")
    name = member[len(prefix) :]
    account = whoami(call)
    if name == account["name"]:
        return viewer_reader(account)
    try:
        call(f"/api/users/{urllib.parse.quote(name, safe='')}/overview")
    except NotFound:
        # The Hub definitively does not know this member, who stays the
        # reader as written.
        return None
    return member


def answer(call, artifact):
    if not isinstance(artifact, dict):
        raise ValueError("the artifact must be an object")
    match sorted(artifact):
        case ["selector"]:
            match artifact["selector"]:
                case "viewer":
                    members = [viewer_reader(whoami(call))]
                case str() as selector:
                    match selector.split("/"):
                        case ["org", org, "resource-group", group, "members"] if org and group:
                            members = group_members(call, org, group)
                        case _:
                            raise ValueError(f"{selector!r} names no collection this source serves")
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

    json.dump({"version": 1, "answer": answer(hub_api(resolve_token()), request.get("artifact"))}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"huggingface audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
