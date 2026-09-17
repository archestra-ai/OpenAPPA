"""The slack audience source: one consult in, one answer out.

Serves these selector templates over the Slack Web API:

  viewer               the token's own reader
  full-members         every full workspace member — no guests, no
                       Slack Connect participants, no bots, no
                       deactivated accounts
  user-group/<handle>  one user group's members, as Slack reports them
  channel/<id>         one conversation's readers: for a public
                       channel every full member (any of them may join
                       it) plus its current members; for a private
                       channel, group DM, or DM its members as Slack
                       reports them; a user id (U... or W...) is the DM
                       with that user, exactly the viewer and that user

and the member lookup that resolves one `slack:U...` member to its
reader.

A member is the account's profile email where Slack marks the address
confirmed, else the qualified `slack:<id>`, which merges with no other
provider's reader.

Credentials come from APPA_PROVIDER_SLACK_TOKEN (a bot or user token with
users:read, users:read.email, usergroups:read, and — for `channel/<id>` —
channels:read, groups:read, im:read, and mpim:read). Any Slack error,
missing answer, or malformed response exits nonzero: the runtime treats
that as no answer and refuses the operation, so a directory hiccup never
becomes a policy decision.
"""

import json
import os
import sys
import urllib.parse
import urllib.request


API_ROOT = "https://slack.com/api/"
TOKEN_VAR = "APPA_PROVIDER_SLACK_TOKEN"
SOURCE_NAME = "slack"
SERVED_TEMPLATES = ["viewer", "full-members", "user-group/<handle>", "channel/<id>"]
TIMEOUT_SECONDS = 30


def web_api(token):
    def call(method, **params):
        request = urllib.request.Request(
            API_ROOT + method,
            data=urllib.parse.urlencode(params).encode("utf-8"),
            headers={
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/x-www-form-urlencoded",
            },
        )
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
            return json.load(response)

    return call


def api_ok(call, method, **params):
    response = call(method, **params)
    if not isinstance(response, dict) or not response.get("ok"):
        error = response.get("error") if isinstance(response, dict) else "malformed response"
        raise RuntimeError(f"{method} failed: {error}")
    return response


def confirmed_email_of(user):
    """The profile address, only where Slack marks it confirmed: an
    unconfirmed address would seat this account on another reader."""
    email = user.get("profile", {}).get("email")
    if isinstance(email, str) and email and user.get("is_email_confirmed"):
        return email
    return None


def reader_of(user):
    email = confirmed_email_of(user)
    return email if email else f"slack:{user['id']}"


# Accounts a listing may return: `users.list` is rate-limited to tens of
# pages a minute, so a larger workspace cannot answer inside the runtime's
# consult budget and is refused instead of timing out halfway; a
# conversation's member list is bounded the same way, before the
# directory pass it would need.
MAX_DIRECTORY = 5000


def paged(call, method, **params):
    """Every item of a cursor-paged Slack listing, page by page, refused
    past the bound."""
    cursor = None
    listed = 0
    while True:
        response = api_ok(call, method, **params, **({"cursor": cursor} if cursor else {}))
        listed += len(response["members"])
        if listed > MAX_DIRECTORY:
            raise RuntimeError(f"{method} lists more than {MAX_DIRECTORY} accounts; map that audience from a bulk source")
        yield from response["members"]
        cursor = response.get("response_metadata", {}).get("next_cursor", "")
        if not cursor:
            return


def list_users(call):
    return list(paged(call, "users.list", limit=200))


def is_full_member(user, team_id):
    if user.get("deleted") or user.get("is_bot") or user.get("is_app_user"):
        return False
    if user.get("id") == "USLACKBOT":
        return False
    # Guests (multi- and single-channel) and Slack Connect participants
    # are in the workspace but are not full members.
    if user.get("is_restricted") or user.get("is_ultra_restricted") or user.get("is_stranger"):
        return False
    # An account Slack does not place in this workspace is not a full
    # member of it; a missing team is not evidence that it is.
    return user.get("team_id") == team_id


def viewer_members(call):
    auth = api_ok(call, "auth.test")
    user = api_ok(call, "users.info", user=auth["user_id"])["user"]
    return [reader_of(user)]


def full_members(call):
    team_id = api_ok(call, "auth.test")["team_id"]
    return [reader_of(user) for user in list_users(call) if is_full_member(user, team_id)]


def user_group_members(call, handle):
    groups = api_ok(call, "usergroups.list")["usergroups"]
    matches = [group for group in groups if group.get("handle") == handle]
    if not matches:
        raise RuntimeError(f"no user group has the handle {handle!r}")
    user_ids = api_ok(call, "usergroups.users.list", usergroup=matches[0]["id"])["users"]
    # One directory pass rather than a lookup per member: a large group
    # would otherwise outrun the runtime's consult timeout.
    directory = {user["id"]: user for user in list_users(call)}
    members = []
    for user_id in user_ids:
        user = directory.get(user_id)
        if user is None:
            raise RuntimeError(f"the directory does not report group member {user_id}")
        if not user.get("deleted"):
            members.append(reader_of(user))
    return members


def conversation_kind(channel_id):
    """What a `channel/<id>` names, from the id Slack issued: a conversation
    Slack lists members for, or a user standing for the DM with that user.
    Anything else names no collection; a name or a URL is never guessed
    into one."""
    match channel_id[:1]:
        case "C" | "G" | "D" if len(channel_id) > 1 and channel_id.isalnum():
            return "conversation"
        case "U" | "W" if len(channel_id) > 1 and channel_id.isalnum():
            return "user"
        case _:
            raise ValueError(f"{channel_id!r} is not a Slack conversation or user id")


def conversation_member_ids(call, channel_id):
    return list(paged(call, "conversations.members", channel=channel_id, limit=200))


# Members looked up one by one before a directory pass is cheaper: a DM or
# a small private channel costs a few `users.info` calls, a large one the
# directory pages a user group costs.
DIRECT_LOOKUPS = 20


def users_by_id(call, user_ids):
    """The directory entries for exactly these ids; an id Slack does not
    report is a failure, never a member silently dropped."""
    if len(user_ids) <= DIRECT_LOOKUPS:
        return {user_id: api_ok(call, "users.info", user=user_id)["user"] for user_id in user_ids}
    directory = {user["id"]: user for user in list_users(call)}
    for user_id in user_ids:
        if user_id not in directory:
            raise RuntimeError(f"the directory does not report conversation member {user_id}")
    return {user_id: directory[user_id] for user_id in user_ids}


VISIBILITY_FLAGS = ("is_private", "is_im", "is_mpim")


def is_private_conversation(conversation):
    """Whether Slack reports the conversation as private, a group DM, or a
    DM; a payload reporting none of the flags is refused, never read as
    a public channel."""
    flags = [conversation[flag] for flag in VISIBILITY_FLAGS if flag in conversation]
    if not flags or not all(isinstance(flag, bool) for flag in flags):
        raise RuntimeError("conversations.info reports no conversation visibility")
    return any(flags)


def channel_members(call, channel_id):
    match conversation_kind(channel_id):
        case "user":
            # The DM with one user is read by exactly the viewer and that user.
            other = api_ok(call, "users.info", user=channel_id)["user"]
            members = viewer_members(call)
            reader = reader_of(other)
            return members if reader in members else members + [reader]
        case "conversation":
            conversation = api_ok(call, "conversations.info", channel=channel_id)["channel"]
            user_ids = conversation_member_ids(call, channel_id)
            if is_private_conversation(conversation):
                users = users_by_id(call, user_ids)
                return [reader_of(users[user_id]) for user_id in user_ids if not users[user_id].get("deleted")]
            # Any full member may join a public channel and read its history,
            # so they all read it, with the guests and Slack Connect
            # participants already in it.
            team_id = api_ok(call, "auth.test")["team_id"]
            directory = list_users(call)
            present = set(user_ids)
            missing = present - {user["id"] for user in directory}
            if missing:
                raise RuntimeError(f"the directory does not report conversation members {sorted(missing)}")
            return [
                reader_of(user)
                for user in directory
                if is_full_member(user, team_id) or (user["id"] in present and not user.get("deleted"))
            ]


def member_principal(call, member):
    prefix = "slack:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a slack-qualified member")
    response = call("users.info", user=member[len(prefix) :])
    if not isinstance(response, dict):
        raise RuntimeError("users.info failed: malformed response")
    if response.get("ok"):
        email = confirmed_email_of(response["user"])
        # Without a confirmed address the member is the reader as
        # written, in the queried spelling.
        return email if email else member
    if response.get("error") == "user_not_found":
        # Slack definitively does not know this member, who stays the
        # reader as written.
        return None
    raise RuntimeError(f"users.info failed: {response.get('error')}")


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
                case str() if selector.startswith("user-group/") and len(selector) > len("user-group/"):
                    members = user_group_members(call, selector[len("user-group/") :])
                case str() if selector.startswith("channel/") and len(selector) > len("channel/"):
                    members = channel_members(call, selector[len("channel/") :])
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

    json.dump({"version": 1, "answer": answer(web_api(token), request.get("artifact"))}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"slack audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
