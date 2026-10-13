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
                                 see otherwise; and what its result returns

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
pull requests (`list_issues`, `list_pull_requests`) keeps the session's
trust only when the provider's answer for that repository holds the
listing, not truncated, and every listed item's author, and its last
editor when it was edited, is the repository's OWNER, MEMBER, or
COLLABORATOR, or a bot; otherwise it is `suspicious`. Other repository
content (files, commits, branches, tags, releases) is pushed by the
repository's writers and merged from pull requests its readers open: a
public repository's readers are anyone, and a fork's content came from
its parent, so both are `suspicious`; a private or internal repository
that is not a fork keeps the session's trust.

A write's result is labelled only when it returns stored text beyond the
call's own arguments. `sub_issue_write` (every method) returns the parent
issue its `issue_number` names, title and body included; its result gets
the label an `issue_read` of that issue gets, so it is `suspicious`
unless the context proves its authorship, on a private repository too.
Every other write
returns identifiers, URLs, SHAs, or a fixed message, and its `delta` is
empty.

A non-public repository's readers are the collection
`@github:repo/<owner>/<repo>/collaborators`, which the `github` audience
source resolves. The policy's mandate for each call names exactly that
spelling; the script refuses a consult whose mandate does not (exit
status 2) before it reads a token.

Credentials come from APPA_PROVIDER_GITHUB_TOKEN, else the GitHub CLI's
login (see github_token.py); a multi-user deployment runs
repository-visibility-multi-user.py, which reads the variable alone; the API root is GITHUB_API_URL when set (a
GitHub Enterprise Server's /api/v3), else api.github.com. A repository
the token cannot see, or any GitHub error, exits nonzero: the runtime
treats that as no answer and refuses the operation, so nothing is
guessed public.
"""

from dataclasses import dataclass
import json
import os
import sys
import urllib.parse

# The sibling module is found beside this file however the file is loaded:
# run by the runtime from its own directory, or imported by path from another.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from github_audience import rest_api  # noqa: E402
from github_token import resolve_token  # noqa: E402


CONTENT = "github.repository-visibility"
READERS = "github.repository-readers"
# create_or_update_file and push_files carry the file content inside the consult.
MAX_INPUT_BYTES = 4 * 1024 * 1024


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
    """The part of the consult that decides a result's trust: the tool, the
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
        number = next((value for value in numbers if isinstance(value, int) and not isinstance(value, bool) and value > 0), None)
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
# Writes whose result returns what people wrote on the item the call names
# (github-mcp-server v2.0.2, pkg/github/issues.go:1847,1885,1941).
ECHOES = {"sub_issue_write": "issue"}
# Reads that return what the authors of many of them wrote.
LISTINGS = {"list_issues": "issues", "list_pull_requests": "pull_requests"}


@dataclass(frozen=True)
class Repository:
    visibility: str
    fork: bool


def repository_facts(call, owner, repo):
    payload = call(f"/repos/{urllib.parse.quote(owner, safe='')}/{urllib.parse.quote(repo, safe='')}")
    visibility = payload.get("visibility") if isinstance(payload, dict) else None
    if visibility not in VISIBILITIES:
        raise RuntimeError(f"GitHub reports the unknown repository visibility {visibility!r}")
    fork = payload.get("fork")
    if not isinstance(fork, bool):
        raise RuntimeError(f"GitHub reports no fork flag for the repository: {fork!r}")
    return Repository(visibility, fork)


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


def context_answer(call, owner, repo):
    """The `github` context answer when it is about this very repository."""
    answer = (call.context or {}).get("answer")
    if not isinstance(answer, dict):
        return None
    named = (answer.get("repository") or {}).get("name")
    return answer if isinstance(named, str) and named.lower() == f"{owner}/{repo}".lower() else None


def identity_key(answer):
    """The key the context answer names each person by: a single-user
    answer names its viewer and every person by `login`; a multi-user one
    has no viewer and names every person `person`, `github:<id>`."""
    match answer:
        case {"viewer": _}:
            return "login"
        case _:
            return "person"


def written_by_the_team(call, owner, repo, kind):
    """Whether the context shows that only the repository's own people wrote
    the pull request or issue (`kind`) the call names."""
    answer = context_answer(call, owner, repo)
    item = answer.get(kind) if answer and call.number is not None else None
    if not isinstance(item, dict):
        return False
    participants = item.get("participants")
    if item.get("number") != call.number or item.get("truncated") is not False or not participants:
        return False
    key = identity_key(answer)
    team = {
        participant.get(key)
        for participant in participants
        if isinstance(participant, dict) and (participant.get("association") in TEAM or participant.get("bot") is True)
    }
    team.discard(None)
    everyone = [participant.get(key) if isinstance(participant, dict) else None for participant in participants]
    author = (item.get("author") or {}).get(key)
    wrote = [author, *everyone, *item.get("commit_authors", []), *item.get("last_editors", [])]
    return all(login in team for login in wrote)


def listed_by_the_team(call, owner, repo):
    """Whether the context shows that only the repository's own people wrote
    and edited every issue or pull request the listing returns."""
    answer = context_answer(call, owner, repo)
    listed = answer.get(LISTINGS[call.tool]) if answer else None
    items = listed.get("items") if isinstance(listed, dict) else None
    if not isinstance(items, list) or listed.get("truncated") is not False or not all(isinstance(item, dict) for item in items):
        return False
    key = identity_key(answer)
    authors = [(item.get("author") if isinstance(item.get("author"), dict) else {}, item.get("bot") is True) for item in items]
    team = {author.get(key) for author, bot in authors if author.get("association") in TEAM or bot}
    team.discard(None)
    wrote = [author.get(key) for author, _ in authors] + [item["last_editor"] for item in items if "last_editor" in item]
    return all(login in team for login in wrote)


def keeps_trust(call, repository, owner, repo):
    """Trust follows the author: see the module docstring."""
    match call.tool:
        case tool if tool in DISCUSSIONS:
            return written_by_the_team(call, owner, repo, DISCUSSIONS[tool])
        case tool if tool in LISTINGS:
            return listed_by_the_team(call, owner, repo)
        case _:
            return repository.visibility != "public" and not repository.fork


def labelled(audience, trusted):
    return {"audience": audience} if trusted else {"trust": "suspicious", "audience": audience}


def read_delta(call, repository, owner, repo):
    return labelled(read_by(repository.visibility, owner, repo), keeps_trust(call, repository, owner, repo))


def write_delta(call, repository, owner, repo):
    """What a write's result returns beyond the call's own arguments: see the
    module docstring."""
    match ECHOES.get(call.tool):
        case None:
            return {}
        case kind:
            return labelled(read_by(repository.visibility, owner, repo), written_by_the_team(call, owner, repo, kind))


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
                "delta": write_delta(call, repository, owner, repo),
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


def main(token):
    """Answers the consult on stdin with the token `token()` returns."""
    try:
        raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
        if len(raw) > MAX_INPUT_BYTES:
            raise ValueError("the consult is too large")
        consult = json.loads(raw)
        name, owner, repo, call = repository_of(consult)
        check_declaration(consult, owner, repo)

        repository = repository_facts(rest_api(token()), owner, repo)
        json.dump({"version": 1, "answer": annotation(name, call, repository, owner, repo)}, sys.stdout)
        sys.stdout.write("\n")
    except Exception as error:
        print(f"github repository annotator: {error}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main(resolve_token)
