"""The Slack Web API as the battery's scripts call it."""

import json
import urllib.parse
import urllib.request


API_ROOT = "https://slack.com/api/"
TOKEN_VAR = "APPA_PROVIDER_SLACK_TOKEN"
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


def conversation_info(call, channel_id):
    """`conversations.info` for one conversation. Slack answers
    `channel_not_found` both for an id it does not know and for a private
    conversation the token's account is not in; a DM is visible only to
    its two ends, so the error names what the deployment can change."""
    try:
        return api_ok(call, "conversations.info", channel=channel_id)["channel"]
    except RuntimeError as error:
        if not str(error).endswith("failed: channel_not_found"):
            raise
        if channel_id.startswith("D"):
            raise RuntimeError(
                f"DM {channel_id} is not visible to the token's account: a DM is visible only to its two ends. "
                "Name the DM by the other person's user id instead, or bind the user token of the account that sends"
            ) from None
        raise RuntimeError(f"conversation {channel_id} does not exist or is not visible to the token's account") from None


def is_person(user):
    """Whether a directory entry is a person's account rather than an app's."""
    return not (user.get("is_bot") or user.get("is_app_user") or user.get("id") == "USLACKBOT")


def conversation_kind(channel_id):
    """What a channel id names, from the id Slack issued: a conversation
    Slack lists members for, or a user standing for the DM with that user.
    Anything else names no conversation; a name or a URL is never guessed
    into one."""
    match channel_id[:1]:
        case "C" | "G" | "D" if len(channel_id) > 1 and channel_id.isalnum():
            return "conversation"
        case "U" | "W" if len(channel_id) > 1 and channel_id.isalnum():
            return "user"
        case _:
            raise ValueError(f"{channel_id!r} is not a Slack conversation or user id")
