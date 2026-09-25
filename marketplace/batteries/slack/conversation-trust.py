"""The slack conversation annotator: one consult in, one answer out.

A read of one conversation is read by that conversation's readers,
`@slack:channel/<id>`. Trust follows who can write the text: a
conversation shared with another organization through Slack Connect, or
a DM with a user from one, carries text written outside the workspace
and enters `suspicious`; any other conversation keeps the session's
trust. Guests and installed integrations write as the workspace.

Slack answers through `conversations.info` for a conversation id and
`users.info` for a user id standing for the DM with that user, with the
scopes the `channel/<id>` audience selector already needs. Where Slack
cannot answer — no token, an error, a timeout — the conversation keeps
the session's trust. A malformed consult, or a policy whose mandate does
not name the conversation's readers, is refused (exit status 2 for the
mandate).
"""

import json
import os
import sys

# The sibling module is found beside this file however the file is loaded:
# run by the runtime from its own directory, or imported by path from another.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from slack_api import TOKEN_VAR, api_ok, conversation_kind, web_api  # noqa: E402


NAME = "slack.conversation-trust"
MAX_INPUT_BYTES = 64 * 1024


def channel_of(consult):
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
    arguments = args.get("arguments") if isinstance(args, dict) else None
    channel_id = arguments.get("channel_id") if isinstance(arguments, dict) else None
    if not isinstance(channel_id, str):
        raise ValueError("the call names no channel_id")
    conversation_kind(channel_id)
    return channel_id


def readers(channel_id):
    return f"@slack:channel/{channel_id}"


def check_declaration(consult, channel_id):
    declared = consult.get("declaration", {}).get("audiences")
    if not isinstance(declared, list) or readers(channel_id) not in declared:
        print(f"{NAME}: the policy admits {declared!r}, this script answers with {readers(channel_id)!r}", file=sys.stderr)
        raise SystemExit(2)


def is_external(call, channel_id):
    match conversation_kind(channel_id):
        case "user":
            return bool(api_ok(call, "users.info", user=channel_id).get("user", {}).get("is_stranger"))
        case "conversation":
            conversation = api_ok(call, "conversations.info", channel=channel_id).get("channel", {})
            return bool(conversation.get("is_ext_shared") or conversation.get("is_pending_ext_shared"))


def established_external(call, channel_id):
    """Whether Slack reports the conversation as shared outside the
    workspace; `False` where Slack cannot answer."""
    if call is None:
        print(f"{NAME}: {TOKEN_VAR} is not set; {channel_id} keeps the session's trust", file=sys.stderr)
        return False
    try:
        return is_external(call, channel_id)
    except (OSError, RuntimeError, ValueError, AttributeError) as error:
        print(f"{NAME}: {error}; {channel_id} keeps the session's trust", file=sys.stderr)
        return False


def annotation(channel_id, external):
    audience = [readers(channel_id)]
    return {
        "delta": {"trust": "suspicious", "audience": audience} if external else {"audience": audience},
        "requires": {"history": [], "attention": []},
        "emits": [],
    }


def main():
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        raise ValueError("the consult is too large")
    consult = json.loads(raw)
    channel_id = channel_of(consult)
    check_declaration(consult, channel_id)
    token = os.environ.get(TOKEN_VAR)
    external = established_external(web_api(token) if token else None, channel_id)
    json.dump({"version": 1, "answer": annotation(channel_id, external)}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"{NAME}: {error}", file=sys.stderr)
        raise SystemExit(1)
