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
