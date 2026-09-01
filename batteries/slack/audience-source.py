"""The slack audience source: one consult in, one answer out.

Serves the stock `slack` selector catalog over the Slack Web API:

  viewer               the token's own principal
  full-members         every full workspace member — no guests, no
                       Slack Connect participants, no bots, no
                       deactivated accounts
  user-group/<handle>  one user group's members, as Slack reports them

and the member lookup that canonicalizes one `slack:U...` reader.

Credentials come from OPENAPPA_SLACK_TOKEN (a bot or user token with
users:read, users:read.email, and usergroups:read). Any Slack error,
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
TOKEN_VAR = "OPENAPPA_SLACK_TOKEN"
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


def verified_email_of(user):
    """The profile address, only where Slack marks it confirmed: an
    unconfirmed address would seat this account on another reader's
    principal."""
    email = user.get("profile", {}).get("email")
    if isinstance(email, str) and email and user.get("is_email_confirmed"):
        return email
    return None


def claims_of(user):
    claims = {"id": f"slack:{user['id']}"}
    email = verified_email_of(user)
    if email:
        claims["verified_email"] = email
    return claims


def list_users(call):
    users = []
    cursor = None
    while True:
        params = {"limit": 200}
        if cursor:
            params["cursor"] = cursor
        response = api_ok(call, "users.list", **params)
        users.extend(response["members"])
        cursor = response.get("response_metadata", {}).get("next_cursor", "")
        if not cursor:
            return users


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
    identity = api_ok(call, "auth.test")
    user = api_ok(call, "users.info", user=identity["user_id"])["user"]
    return [claims_of(user)]


def full_members(call):
    team_id = api_ok(call, "auth.test")["team_id"]
    return [claims_of(user) for user in list_users(call) if is_full_member(user, team_id)]


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
            members.append(claims_of(user))
    return members


def member_claims(call, member):
    prefix = "slack:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a slack-qualified member")
    response = call("users.info", user=member[len(prefix) :])
    if not isinstance(response, dict):
        raise RuntimeError("users.info failed: malformed response")
    if response.get("ok"):
        # The claims echo the queried spelling: claims for another id
        # are refused.
        claims = {"id": member}
        email = verified_email_of(response["user"])
        if email:
            claims["verified_email"] = email
        return claims
    if response.get("error") == "user_not_found":
        # Slack definitively does not know this member, who keeps the
        # qualified identity.
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
    if request.get("name") != "slack":
        raise ValueError("unexpected source name")

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
