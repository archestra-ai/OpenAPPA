"""The archestra audience source: one consult in, one answer out.

Serves these selector templates over Archestra's audience API:

  members          every member of the organization the token belongs to
  team/<team>      one team, named by its id or its name
  user/<user>      one member, named by their user id or email

and the member lookup that resolves one `archestra:<user-id>` member to
that user's email.

Every member is reported as the email address Archestra holds for the
account, lowercased — the address the identity provider signs the user
in with — so an Archestra reader compares with the readers any other
email-keyed source reports.

The API base comes from ARCHESTRA_BASE_URL, and credentials from
APPA_PROVIDER_ARCHESTRA_TOKEN: an Archestra API key allowed to read the
organization's membership. Any API error or missing answer exits
nonzero: the runtime treats that as no answer and refuses the operation,
so a directory hiccup never becomes a policy decision.
"""

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request


BASE_URL_VAR = "ARCHESTRA_BASE_URL"
TOKEN_VAR = "APPA_PROVIDER_ARCHESTRA_TOKEN"
SOURCE_NAME = "archestra"
SERVED_TEMPLATES = ["members", "team/<team>", "user/<user>"]
TIMEOUT_SECONDS = 30


def audience_api(base_url, token):
    endpoint = f"{base_url.rstrip('/')}/api/openappa/audience"

    def call(**params):
        request = urllib.request.Request(
            f"{endpoint}?{urllib.parse.urlencode(params)}",
            headers={"Authorization": f"Bearer {token}"},
        )
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"GET {endpoint} failed: {error.code}") from error

    return call


def served(selector):
    match selector.split("/"):
        case ["members"]:
            return True
        case ["team" | "user", name]:
            return bool(name)
        case _:
            return False


def members(call, selector):
    if not isinstance(selector, str) or not served(selector):
        raise ValueError(f"{selector!r} names no collection this source serves")
    found = call(selector=selector).get("members")
    if not isinstance(found, list) or not all(isinstance(member, str) and member for member in found):
        raise RuntimeError(f"the answer for {selector!r} carries no member list")
    return [member.lower() for member in found]


def member_principal(call, member):
    prefix = "archestra:"
    if not isinstance(member, str) or not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not an archestra-qualified member")
    answer = call(member=member[len(prefix) :])
    if "principal" not in answer:
        raise RuntimeError(f"the answer for {member!r} carries no principal")
    principal = answer["principal"]
    match principal:
        case None:
            # Archestra definitively does not know this user, who stays the
            # reader as written.
            return None
        case str() if principal:
            return principal.lower()
        case _:
            raise RuntimeError(f"the answer for {member!r} carries no principal")


def answer(call, artifact):
    if not isinstance(artifact, dict):
        raise ValueError("the artifact must be an object")
    match sorted(artifact):
        case ["selector"]:
            return {"members": members(call, artifact["selector"])}
        case ["member"]:
            return {"principal": member_principal(call, artifact["member"])}
        case _:
            raise ValueError("the artifact must carry exactly a selector or a member")


def check_declaration(request):
    """The policy's declared templates against the ones this script serves.

    A mismatch is a version skew between policy and script, refused before
    any credential is read; the exit status 2 tells it apart from a
    provider failure.
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

    base_url = os.environ.get(BASE_URL_VAR)
    if not base_url:
        raise RuntimeError(f"{BASE_URL_VAR} is not set")
    token = os.environ.get(TOKEN_VAR)
    if not token:
        raise RuntimeError(f"{TOKEN_VAR} is not set")

    json.dump({"version": 1, "answer": answer(audience_api(base_url, token), request.get("artifact"))}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"archestra audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
