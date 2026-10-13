"""The github audience source of a multi-user deployment: one consult in,
one answer out.

The token is a GitHub App installation's, which has no user behind it:
there is no viewer, and no profile or email it may read for a person.
Serves these selector templates over the GitHub REST API:

  org/<org>/members       one explicitly selected organization's members
  org/<org>/team/<team>   one organization team, by slug
  repo/<owner>/<repo>/collaborators
                          one repository's collaborators

Every member is `github:<id>`, the account's numeric id from the listing
itself: an id survives a renamed login, and the deployment maps it to a
person by its own lookup entry, so this source answers no member lookup.
Bots are left out, as in the single-user source.

Credentials come from APPA_PROVIDER_GITHUB_TOKEN alone; the API root is
GITHUB_API_URL when set, else api.github.com. Any GitHub error or
missing answer exits nonzero.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from github_audience import collection_path, people, serve  # noqa: E402
from github_token import env_token  # noqa: E402


SOURCE_NAME = "github"
SERVED_TEMPLATES = [
    "org/<org>/members",
    "org/<org>/team/<team>",
    "repo/<owner>/<repo>/collaborators",
]
# Listing pages a collection may fill. Members need no profile reads here,
# so the budget is wider than the single-user source's, but a roster past
# it is refused rather than answered in part.
MAX_PAGES = 50


def person(account):
    match account.get("id"):
        case int() as number if not isinstance(number, bool) and number > 0:
            return f"github:{number}"
        case other:
            raise RuntimeError(f"GitHub lists an account without a numeric id: {other!r}")


def answer(call, artifact):
    match artifact:
        case {"selector": str() as selector} if len(artifact) == 1:
            return {"members": [person(account) for account in people(call, collection_path(selector), MAX_PAGES)]}
        case {"selector": _} if len(artifact) == 1:
            raise ValueError("the selector must be a string")
        case {"member": _} if len(artifact) == 1:
            raise ValueError("this source answers no member lookup; the deployment routes them to its lookup entry")
        case _:
            raise ValueError("the artifact must carry exactly a selector")


if __name__ == "__main__":
    serve(SOURCE_NAME, SERVED_TEMPLATES, answer, env_token)
