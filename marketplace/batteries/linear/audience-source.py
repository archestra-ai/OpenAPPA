"""The linear audience source: one consult in, one answer out.

Serves these selector templates over the Linear GraphQL API:

  viewer                 the token's own reader
  full-members           every active member of the workspace — no
                         guests, no app users
  team/<key>/members     one team's members, as Linear reports them
  team/<key>/readers     who can see the team's issues, projects and
                         cycles: every full member for a public team,
                         its members for a private team, its members and
                         the parent team's readers for a restricted
                         sub-team
  issue/<id>/readers     the team's readers plus the people the issue
                         is shared with individually
  project/<id>/readers   the readers of every team the project belongs
                         to, plus the project's own members
  document/<id>/readers  the readers of the issue, project, or team the
                         document belongs to; every full member for a
                         document under an initiative

and the member lookup that resolves one `linear:<id>` member to its
reader.

A team is named by its UUID or its key (`ENG`); an issue by its UUID or
identifier (`ENG-123`); a project and a document by their UUID or slug.
A name is never looked up: two things can share one, and a guessed
match would seat readers on the wrong resource.

A member is the account's email as Linear reports it, else the
qualified `linear:<id>`, which merges with no other provider's reader.
Deactivated accounts and app users are never members.

Credentials come from APPA_PROVIDER_LINEAR_TOKEN (a personal API key
or an OAuth access token that can read users, teams, issues, projects,
and documents). Any Linear error, missing answer, or malformed response
exits nonzero: the runtime treats that as no answer and refuses the
operation, so an API hiccup never becomes a policy decision.
"""

import functools
import json
import os
import re
import sys
import urllib.request


API_URL = "https://api.linear.app/graphql"
TOKEN_VAR = "APPA_PROVIDER_LINEAR_TOKEN"
SOURCE_NAME = "linear"
SERVED_TEMPLATES = [
    "viewer",
    "full-members",
    "team/<key>/members",
    "team/<key>/readers",
    "issue/<id>/readers",
    "project/<id>/readers",
    "document/<id>/readers",
]
TIMEOUT_SECONDS = 30
PAGE_SIZE = 250
UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")

USER_FIELDS = "id email active guest app"
TEAM_FIELDS = "id key visibility parent { id }"


class NotFound(Exception):
    """Linear answered that the entity does not exist."""


def graphql(token):
    def call(query, **variables):
        request = urllib.request.Request(
            API_URL,
            data=json.dumps({"query": query, "variables": variables}).encode("utf-8"),
            headers={"Authorization": token, "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
            body = json.load(response)
        if not isinstance(body, dict):
            raise RuntimeError("malformed response")
        errors = body.get("errors")
        if errors:
            messages = "; ".join(str(error.get("message")) for error in errors)
            if any(str(error.get("message", "")).startswith("Entity not found") for error in errors):
                raise NotFound(messages)
            raise RuntimeError(messages)
        return body["data"]

    return call


def reader_of(user):
    email = user.get("email")
    return email if isinstance(email, str) and email else f"linear:{user['id']}"


def is_person(user):
    """A member is a person with an active account: an app user is an
    integration, a deactivated account reads nothing any more."""
    return bool(user.get("active")) and not user.get("app")


# Nodes one connection may hold: a larger roster cannot answer inside the
# runtime's consult budget, so it is refused instead of timing out halfway.
MAX_NODES = 5000


def paginated(call, query, path, **variables):
    """Every node of one connection, `path` naming it under `data`."""
    nodes = []
    after = None
    while True:
        data = call(query, first=PAGE_SIZE, after=after, **variables)
        connection = data
        for key in path:
            connection = connection[key]
        nodes.extend(connection["nodes"])
        if len(nodes) > MAX_NODES:
            raise RuntimeError(f"{'.'.join(path)} holds more than {MAX_NODES} nodes; map that audience from a bulk source")
        page = connection["pageInfo"]
        if not page["hasNextPage"]:
            return nodes
        after = page["endCursor"]


def viewer_members(call):
    user = call("{ viewer { %s } }" % USER_FIELDS)["viewer"]
    return [reader_of(user)]


# One consult is one process: the roster is fetched once however many
# public teams a project or a restricted team's ancestry spans.
@functools.cache
def full_members(call):
    users = paginated(
        call,
        "query($first: Int!, $after: String) { users(first: $first, after: $after) { nodes { %s } pageInfo { hasNextPage endCursor } } }"
        % USER_FIELDS,
        ["users"],
    )
    return [reader_of(user) for user in users if is_person(user) and not user.get("guest")]


def team_by(call, spelling):
    """One team by UUID or key, exactly one match."""
    if UUID.match(spelling):
        return call("query($id: String!) { team(id: $id) { %s } }" % TEAM_FIELDS, id=spelling)["team"]
    teams = call(
        "query($key: String!) { teams(filter: { key: { eq: $key } }) { nodes { %s } } }" % TEAM_FIELDS,
        key=spelling,
    )["teams"]["nodes"]
    if len(teams) != 1:
        raise NotFound(f"no single team has the key {spelling!r}")
    return teams[0]


def team_members(call, team):
    users = paginated(
        call,
        "query($id: String!, $first: Int!, $after: String) { team(id: $id) { members(first: $first, after: $after) { nodes { %s } pageInfo { hasNextPage endCursor } } } }"
        % USER_FIELDS,
        ["team", "members"],
        id=team["id"],
    )
    return [reader_of(user) for user in users if is_person(user)]


def team_readers(call, team):
    match team.get("visibility"):
        case "public":
            return full_members(call)
        case "private":
            return team_members(call, team)
        case "restricted":
            # A restricted sub-team is visible to its parent team's readers.
            parent = team.get("parent")
            if not parent:
                raise RuntimeError(f"restricted team {team['id']} reports no parent team")
            return union(team_members(call, team), team_readers(call, team_by(call, parent["id"])))
        case other:
            raise RuntimeError(f"team {team['id']} has the unknown visibility {other!r}")


def union(*collections):
    return list(dict.fromkeys(member for collection in collections for member in collection))


def issue_readers(call, spelling):
    issue = call(
        "query($id: String!) { issue(id: $id) { id team { %s } sharedAccess { sharedWithUsers { %s } } } }"
        % (TEAM_FIELDS, USER_FIELDS),
        id=spelling,
    )["issue"]
    shared = issue.get("sharedAccess", {}).get("sharedWithUsers") or []
    return union(
        team_readers(call, issue["team"]),
        [reader_of(user) for user in shared if is_person(user)],
    )


def project_by(call, spelling):
    if UUID.match(spelling):
        return call("query($id: String!) { project(id: $id) { id } }", id=spelling)["project"]
    projects = call(
        "query($slug: String!) { projects(filter: { slugId: { eq: $slug } }) { nodes { id } } }",
        slug=spelling,
    )["projects"]["nodes"]
    if len(projects) != 1:
        raise NotFound(f"no single project has the slug {spelling!r}")
    return projects[0]


def project_readers(call, spelling):
    project = project_by(call, spelling)
    teams = paginated(
        call,
        "query($id: String!, $first: Int!, $after: String) { project(id: $id) { teams(first: $first, after: $after) { nodes { %s } pageInfo { hasNextPage endCursor } } } }"
        % TEAM_FIELDS,
        ["project", "teams"],
        id=project["id"],
    )
    members = paginated(
        call,
        "query($id: String!, $first: Int!, $after: String) { project(id: $id) { members(first: $first, after: $after) { nodes { %s } pageInfo { hasNextPage endCursor } } } }"
        % USER_FIELDS,
        ["project", "members"],
        id=project["id"],
    )
    return union(
        *[team_readers(call, team) for team in teams],
        [reader_of(user) for user in members if is_person(user)],
    )


def document_readers(call, spelling):
    document = call(
        "query($id: String!) { document(id: $id) { id issue { id } project { id } team { %s } initiative { id } } }"
        % TEAM_FIELDS,
        id=spelling,
    )["document"]
    if document.get("issue"):
        return issue_readers(call, document["issue"]["id"])
    if document.get("project"):
        return project_readers(call, document["project"]["id"])
    if document.get("team"):
        return team_readers(call, document["team"])
    if document.get("initiative"):
        # Initiatives are workspace-wide.
        return full_members(call)
    raise RuntimeError(f"document {document['id']} belongs to no issue, project, team, or initiative")


def member_principal(call, member):
    prefix = "linear:"
    if not member.startswith(prefix) or member == prefix:
        raise ValueError(f"{member!r} is not a linear-qualified member")
    user_id = member[len(prefix) :]
    if not UUID.match(user_id):
        raise ValueError(f"{member!r} does not carry a Linear user id")
    try:
        user = call("query($id: String!) { user(id: $id) { %s } }" % USER_FIELDS, id=user_id)["user"]
    except NotFound:
        # Linear definitively does not know this member, who stays the
        # reader as written.
        return None
    return reader_of(user)


def selector_members(call, selector):
    """The members of one served collection, or a ValueError for a
    selector this source does not serve."""
    match selector.split("/") if isinstance(selector, str) else None:
        case ["viewer"]:
            return viewer_members(call)
        case ["full-members"]:
            return full_members(call)
        case ["team", key, "members"] if key:
            return team_members(call, team_by(call, key))
        case ["team", key, "readers"] if key:
            return team_readers(call, team_by(call, key))
        case ["issue", issue, "readers"] if issue:
            return issue_readers(call, issue)
        case ["project", project, "readers"] if project:
            return project_readers(call, project)
        case ["document", document, "readers"] if document:
            return document_readers(call, document)
        case _:
            raise ValueError(f"{selector!r} names no collection this source serves")


def answer(call, artifact):
    if not isinstance(artifact, dict):
        raise ValueError("the artifact must be an object")
    match sorted(artifact):
        case ["selector"]:
            return {"members": selector_members(call, artifact["selector"])}
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

    json.dump({"version": 1, "answer": answer(graphql(token), request.get("artifact"))}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"linear audience source: {error}", file=sys.stderr)
        raise SystemExit(1)
