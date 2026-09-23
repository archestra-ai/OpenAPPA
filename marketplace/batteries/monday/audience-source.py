"""Resolve one monday notification recipient to a confirmed reader.

The `user/<id>` selector is answered from the monday Users API. A missing,
inactive, or unconfirmed user is refused, so a notification cannot use an
unverified email to satisfy its audience requirement.
"""

import json
import os
import re
import sys
import urllib.request


API_URL = "https://api.monday.com/v2"
API_VERSION = "2026-07"
TOKEN_VAR = "APPA_PROVIDER_MONDAY_TOKEN"
SOURCE_NAME = "monday"
SERVED_TEMPLATES = ["user/<id>"]
USER_ID = re.compile(r"^[0-9]+$")
QUERY = "query($ids: [ID!]) { users(ids: $ids) { id email is_email_confirmed status } }"


def graphql(token):
    def call(user_id):
        request = urllib.request.Request(
            API_URL,
            data=json.dumps({"query": QUERY, "variables": {"ids": [user_id]}}).encode("utf-8"),
            headers={
                "Authorization": token,
                "Content-Type": "application/json",
                "API-Version": API_VERSION,
            },
        )
        with urllib.request.urlopen(request, timeout=30) as response:
            body = json.load(response)
        if not isinstance(body, dict) or body.get("errors") or not isinstance(body.get("data"), dict):
            raise RuntimeError("monday user lookup failed")
        return body["data"].get("users")

    return call


def answer(call, artifact):
    if not isinstance(artifact, dict) or sorted(artifact) != ["selector"]:
        raise ValueError("the artifact must carry exactly a selector")
    selector = artifact["selector"]
    if not isinstance(selector, str) or not selector.startswith("user/"):
        raise ValueError("the selector is not a monday user")
    user_id = selector[len("user/") :]
    if not USER_ID.fullmatch(user_id):
        raise ValueError("the selector must contain a numeric monday user id")

    users = call(user_id)
    if not isinstance(users, list) or len(users) != 1 or not isinstance(users[0], dict):
        raise RuntimeError("monday did not return exactly one user")
    user = users[0]
    if str(user.get("id")) != user_id:
        raise RuntimeError("monday returned a different user")
    if user.get("status") != "ACTIVE" or user.get("is_email_confirmed") is not True:
        raise RuntimeError("monday user is inactive or has no confirmed email")
    email = user.get("email")
    if not isinstance(email, str) or not email or "@" not in email:
        raise RuntimeError("monday user has no usable email")
    return {"members": [email]}


def check_declaration(request):
    declared = request.get("declaration", {}).get("templates")
    if declared != SERVED_TEMPLATES:
        print(
            f"monday audience source: the policy declares {declared!r}, this script serves {SERVED_TEMPLATES!r}",
            file=sys.stderr,
        )
        raise SystemExit(2)


def main():
    request = json.load(sys.stdin)
    if request.get("version") != 1 or request.get("kind") != "audience" or request.get("name") != SOURCE_NAME:
        raise ValueError("invalid monday audience consult envelope")
    check_declaration(request)

    token = os.environ.get(TOKEN_VAR)
    if not token:
        raise RuntimeError(f"{TOKEN_VAR} is not set")
    json.dump({"version": 1, "answer": answer(graphql(token), request.get("artifact"))}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"monday audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
