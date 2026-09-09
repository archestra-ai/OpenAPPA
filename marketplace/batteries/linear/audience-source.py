"""Linear audience consults. Reader IDs stay Linear-qualified; no email guessing.

Queries verified against linear/linear's SDK schema at
716871f2042cee9495220276b8ca28b0c35343f4. Membership is not a resource ACL.
"""

from __future__ import annotations

import json
import os
import re
import sys
from urllib.request import Request, build_opener, HTTPRedirectHandler

TOKEN_ENV = "APPA_PROVIDER_LINEAR_TOKEN"
API = "https://api.linear.app/graphql"
MAX_BYTES = 1024 * 1024
MAX_PAGES = 100
ID = re.compile(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
FIELDS = "id active guest"
PAGE = "pageInfo { hasNextPage endCursor } nodes { " + FIELDS + " }"
VIEWER = "query AppaViewer { viewer { " + FIELDS + " } }"
WORKSPACE = "query AppaMembers($after: String) { organization { id users(first: 100, after: $after) { " + PAGE + " } } }"
TEAM = "query AppaTeam($id: String!, $after: String) { team(id: $id) { id members(first: 100, after: $after) { " + PAGE + " } } }"


class Refusal(ValueError):
    """No complete, trustworthy answer is available."""


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise Refusal("Linear API redirected the request")


def identifier(value):
    if not isinstance(value, str) or not ID.fullmatch(value):
        raise Refusal("expected a Linear UUID")
    return value.lower()


def reader(user):
    if not isinstance(user, dict):
        raise Refusal("invalid user object")
    for key in ("active", "guest"):
        if type(user.get(key)) is not bool:
            raise Refusal("incomplete user membership flags")
    return "linear:" + identifier(user.get("id"))


def graphql(token, opener=None):
    if not token or "\n" in token or "\r" in token:
        raise Refusal("missing or invalid " + TOKEN_ENV)
    # Linear personal API keys use the raw key; OAuth tokens use Bearer.
    authorization = token if token.startswith("lin_api_") else "Bearer " + token
    client = opener or build_opener(NoRedirect())

    def call(query, variables):
        request = Request(API, data=json.dumps({"query": query, "variables": variables}).encode(), headers={
            "Authorization": authorization, "Content-Type": "application/json",
            "User-Agent": "OpenAPPA-Linear-audience",
        })
        try:
            with client.open(request, timeout=15) as response:
                raw = response.read(MAX_BYTES + 1)
            if len(raw) > MAX_BYTES:
                raise Refusal("Linear API response exceeds size limit")
            payload = json.loads(raw)
        except Refusal:
            raise
        except Exception:
            # API errors may contain query values or credentials: do not echo them.
            raise Refusal("Linear API request failed") from None
        if not isinstance(payload, dict) or payload.get("errors") or not isinstance(payload.get("data"), dict):
            raise Refusal("Linear API returned errors or incomplete data")
        return payload["data"]

    return call


def members(call, scope, scope_id):
    scope_id = identifier(scope_id)
    cursor = None
    cursors = set()
    seen = set()
    result = []
    for _ in range(MAX_PAGES):
        query = WORKSPACE if scope == "workspace" else TEAM
        variables = {"after": cursor}
        if scope == "team":
            variables["id"] = scope_id
        data = call(query, variables)
        root = data.get("organization" if scope == "workspace" else "team")
        if not isinstance(root, dict) or identifier(root.get("id")) != scope_id:
            raise Refusal("Linear returned a different or inaccessible scope")
        connection = root.get("users" if scope == "workspace" else "members")
        if not isinstance(connection, dict) or not isinstance(connection.get("nodes"), list):
            raise Refusal("incomplete membership page")
        for user in connection["nodes"]:
            principal = reader(user)
            if principal in seen:
                raise Refusal("duplicate member across membership pages")
            seen.add(principal)
            # Guests can belong to a named team, but never imply internal membership.
            if user["active"] and (scope == "team" or not user["guest"]):
                result.append(principal)
        page = connection.get("pageInfo")
        if not isinstance(page, dict) or type(page.get("hasNextPage")) is not bool:
            raise Refusal("missing pagination evidence")
        if not page["hasNextPage"]:
            return sorted(result)
        cursor = page.get("endCursor")
        if not isinstance(cursor, str) or not cursor or cursor in cursors:
            raise Refusal("invalid or repeated membership cursor")
        cursors.add(cursor)
    raise Refusal("membership exceeds pagination limit")


def answer(call, artifact):
    if not isinstance(artifact, dict):
        raise Refusal("artifact must be an object")
    if set(artifact) == {"member"}:
        member = artifact["member"]
        if not isinstance(member, str) or not member.startswith("linear:"):
            raise Refusal("member must belong to Linear")
        identifier(member.removeprefix("linear:"))
        # This source supplies no cross-provider identity attestation. Operators
        # can redirect lookups to a roster; null retains the original reader.
        return {"principal": None}
    if set(artifact) != {"selector"} or not isinstance(artifact["selector"], str):
        raise Refusal("artifact must name exactly one selector or member")
    selector = artifact["selector"]
    if selector == "viewer":
        user = call(VIEWER, {}).get("viewer")
        principal = reader(user)
        if not user["active"]:
            raise Refusal("viewer is inactive")
        return {"members": [principal]}
    parts = selector.split("/")
    if len(parts) == 3 and parts[0] in ("workspace", "team") and parts[2] == "members":
        return {"members": members(call, parts[0], parts[1])}
    raise Refusal("unsupported Linear selector")


def main():
    try:
        raw = sys.stdin.buffer.read(MAX_BYTES + 1)
        if len(raw) > MAX_BYTES:
            raise Refusal("consult exceeds size limit")
        request = json.loads(raw)
        if (not isinstance(request, dict) or type(request.get("version")) is not int
                or request["version"] != 1 or request.get("kind") != "audience"
                or request.get("name") != "linear"):
            raise Refusal("unsupported audience consult")
        artifact = request.get("artifact")
        # Qualified member lookups need no API access.
        call = None if isinstance(artifact, dict) and set(artifact) == {"member"} else graphql(os.environ.get(TOKEN_ENV))
        result = answer(call, artifact)
        json.dump({"version": 1, "answer": result}, sys.stdout)
        sys.stdout.write("\n")
        return 0
    except (ValueError, TypeError, KeyError):
        print("linear audience source: consult refused (invalid input or unavailable membership)", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
