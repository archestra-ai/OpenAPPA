"""The github repository annotators: one consult in, one answer out.

Two annotators share this script, told apart by the consult's name. Both
read the repository the call names (`owner`, `repo`) and ask GitHub for
its visibility:

  github.repository-visibility   what a read returns: content everyone
                                 reads for a public repository, the
                                 repository's collaborators otherwise; its
                                 trust follows who wrote it (below)
  github.repository-readers      what a write needs: trusted data that
                                 everyone may see for a public repository,
                                 that the repository's collaborators may
                                 see otherwise

Only `public` is public. A `private` repository is read by its
collaborators. An Enterprise `internal` one is read by every enterprise
member, a collection this source cannot list: what is read from it is
answered as its collaborators (the narrower bound), and a write into it
needs data everyone may see, since anything narrower could reach an
enterprise member outside the collaborators. A visibility GitHub does
not report is refused.
Trust follows the author, never the visibility alone. A pull request or
an issue (`pull_request_read`, `issue_read`) keeps the session's trust
only when the `github` context provider's answer for that very item shows
every author, commenter, reviewer, editor, and commit author is the
repository's OWNER, MEMBER, or COLLABORATOR, or a bot — a GitHub App
installed on the repository writes as its people — and no list was
truncated; an outsider, a truncated list, an error
entry, or no context at all makes it `suspicious`. A listing of issues or
pull requests names no single item and is `suspicious`. Other repository
content (files, commits, branches, tags, releases) is pushed by the
repository's writers and merged from pull requests its readers open: a
public repository's readers are anyone, and a fork's content came from
its parent, so both are `suspicious`; a private or internal repository
that is not a fork keeps the session's trust.

A non-public repository's readers are the collection
`@github:repo/<owner>/<repo>/collaborators`, which the `github` audience
source resolves. The policy's mandate for each call names exactly that
spelling; the script refuses a consult whose mandate does not (exit
status 2) before it reads a token.

Credentials come from APPA_PROVIDER_GITHUB_TOKEN, else the GitHub CLI's
login (see github_token.py); the API root is GITHUB_API_URL when set (a
GitHub Enterprise Server's /api/v3), else api.github.com. A repository
the token cannot see, or any GitHub error, exits nonzero: the runtime
treats that as no answer and refuses the operation, so nothing is
guessed public.
"""

from dataclasses import dataclass
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

# The sibling module is found beside this file however the file is loaded:
# run by the runtime from its own directory, or imported by path from another.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from github_token import api_root, resolve_token  # noqa: E402


API_ROOT = api_root(os.environ)
CONTENT = "github.repository-visibility"
READERS = "github.repository-readers"
TIMEOUT_SECONDS = 30
# create_or_update_file and push_files carry the file content inside the consult.
MAX_INPUT_BYTES = 4 * 1024 * 1024


def rest_api(token):
    def call(path):
        request = urllib.request.Request(
            f"{API_ROOT}{path}",
            headers={
                "Authorization": f"Bearer {token}",
                "Accept": "application/vnd.github+json",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise RuntimeError(f"GET {path} failed: {error.code}") from error

    return call


def repository_of(consult):
    """The annotator asked, the repository the call names, and the call itself."""
    if not isinstance(consult, dict):
        raise ValueError("the consult must be an object")
    if consult.get("version") != 1:
        raise ValueError("unsupported request version")
    if consult.get("kind") != "annotation":
        raise ValueError("unexpected consult kind")
    name = consult.get("name")
    if name not in (CONTENT, READERS):
        raise ValueError(f"unexpected annotator name {name!r}")
    artifact = consult.get("artifact")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    arguments = args.get("arguments") if isinstance(args, dict) else None
    if not isinstance(arguments, dict):
        raise ValueError("the call's arguments are missing")
    owner = arguments.get("owner")
    repo = arguments.get("repo")
    for label, value in (("owner", owner), ("repo", repo)):
        # The value must spell one path segment of the collection: what the
        # policy's placeholder admits and what GitHub names a repository by.
        if not isinstance(value, str) or not value or "/" in value or value.startswith("$"):
            raise ValueError(f"{label} must be a non-empty string naming one repository segment")
    return name, owner, repo, Call.of(args, artifact.get("context"))


@dataclass(frozen=True)
class Call:
    """The part of the consult that decides a read's trust: the tool, the
    pull request or issue number it names, and the `github` context entry."""

    tool: str
    number: int | None
    context: dict | None

    @staticmethod
    def of(args, context):
        name = args.get("name")
        tool = name.rsplit("/", 1)[-1] if isinstance(name, str) else ""
        arguments = args["arguments"]
        numbers = (arguments.get(key) for key in NUMBER_ARGUMENTS)
        number = next((value for value in numbers if isinstance(value, int) and not isinstance(value, bool)), None)
        entry = context.get("github") if isinstance(context, dict) else None
        return Call(tool, number, entry if isinstance(entry, dict) else None)


def collaborators(owner, repo):
    return f"@github:repo/{owner}/{repo}/collaborators"


def check_declaration(consult, owner, repo):
    """The mandate the policy declared for this call against the one
    collection this script may answer with: the placeholder instantiated
    for the repository the call names. A mismatch is a policy and a script
    of different versions, refused before any credential is read; the
    exit status 2 tells it apart from a provider failure."""
    declared = consult.get("declaration", {}).get("audiences")
    expected = collaborators(owner, repo)
    if not isinstance(declared, list) or expected not in declared:
        print(
            f"{consult.get('name')}: the policy admits {declared!r}, this script answers with {expected!r}",
            file=sys.stderr,
        )
        raise SystemExit(2)


VISIBILITIES = ("public", "private", "internal")
# Authors whose text keeps the session's trust: the repository's own people.
# A bot is a GitHub App installed on the repository, so it writes as them.
TEAM = ("OWNER", "MEMBER", "COLLABORATOR")
NUMBER_ARGUMENTS = ("pullNumber", "pull_number", "issue_number", "issueNumber")
# Reads that return what people wrote on one pull request or issue.
DISCUSSIONS = {"pull_request_read": "pull_request", "issue_read": "issue"}
# Reads that return what many people wrote on many of them.
LISTINGS = {"list_issues", "list_pull_requests"}


@dataclass(frozen=True)
class Repository:
    visibility: str
    fork: bool


def repository_facts(call, owner, repo):
    payload = call(f"/repos/{urllib.parse.quote(owner, safe='')}/{urllib.parse.quote(repo, safe='')}")
    visibility = payload.get("visibility") if isinstance(payload, dict) else None
    if visibility not in VISIBILITIES:
        raise RuntimeError(f"GitHub reports the unknown repository visibility {visibility!r}")
    return Repository(visibility, payload.get("fork") is True)


def read_by(visibility, owner, repo):
    """The narrowest readers this source can name for what the repository
    holds, in the written-audience grammar: the bare `public` token, or a
    list naming the collaborators collection."""
    match visibility:
        case "public":
            return "public"
        case "private" | "internal":
            return [collaborators(owner, repo)]
        case _:
            raise ValueError(f"unexpected repository visibility {visibility!r}")


def must_reach(visibility, owner, repo):
    """Everyone a write into the repository reaches: its collaborators for a
    private one, everyone otherwise — an Enterprise-internal repository is
    read by members this source cannot list."""
    match visibility:
        case "public" | "internal":
            return "public"
        case "private":
            return [collaborators(owner, repo)]
        case _:
            raise ValueError(f"unexpected repository visibility {visibility!r}")


def written_by_the_team(call, owner, repo):
    """Whether the context shows that only the repository's own people wrote
    the pull request or issue the call reads."""
    answer = (call.context or {}).get("answer")
    if not isinstance(answer, dict) or call.number is None:
        return False
    named = (answer.get("repository") or {}).get("name")
    item = answer.get(DISCUSSIONS[call.tool])
    if not isinstance(named, str) or named.lower() != f"{owner}/{repo}".lower() or not isinstance(item, dict):
        return False
    participants = item.get("participants")
    if item.get("number") != call.number or item.get("truncated") is not False or not participants:
        return False
    team = {
        participant.get("login")
        for participant in participants
        if isinstance(participant, dict) and (participant.get("association") in TEAM or participant.get("bot") is True)
    }
    team.discard(None)
    everyone = [participant.get("login") if isinstance(participant, dict) else None for participant in participants]
    author = (item.get("author") or {}).get("login")
    wrote = [author, *everyone, *item.get("commit_authors", []), *item.get("last_editors", [])]
    return all(login in team for login in wrote)


def keeps_trust(call, repository, owner, repo):
    """Trust follows the author: see the module docstring."""
    match call.tool:
        case tool if tool in DISCUSSIONS:
            return written_by_the_team(call, owner, repo)
        case tool if tool in LISTINGS:
            return False
        case _:
            return repository.visibility != "public" and not repository.fork


def read_delta(call, repository, owner, repo):
    audience = read_by(repository.visibility, owner, repo)
    if keeps_trust(call, repository, owner, repo):
        return {"audience": audience}
    return {"trust": "suspicious", "audience": audience}


def annotation(name, call, repository, owner, repo):
    visibility = repository.visibility
    match name:
        case "github.repository-visibility":
            return {
                "delta": read_delta(call, repository, owner, repo),
                "requires": {"history": [], "attention": []},
                "emits": [],
            }
        case "github.repository-readers":
            return {
                "delta": {},
                "requires": {
                    "trust": "trusted",
                    "audience": {"contains": must_reach(visibility, owner, repo)},
                    "history": [],
                    "attention": [],
                },
                "emits": [],
            }
        case _:
            raise ValueError(f"unexpected annotator name {name!r}")


def main():
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        raise ValueError("the consult is too large")
    consult = json.loads(raw)
    name, owner, repo, call = repository_of(consult)
    check_declaration(consult, owner, repo)

    repository = repository_facts(rest_api(resolve_token()), owner, repo)
    json.dump({"version": 1, "answer": annotation(name, call, repository, owner, repo)}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"github repository annotator: {error}", file=sys.stderr)
        raise SystemExit(1)
