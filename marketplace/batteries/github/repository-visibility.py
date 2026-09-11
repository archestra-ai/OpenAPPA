"""The github repository annotators: one consult in, one answer out.

Two annotators share this script, told apart by the consult's name. Both
read the repository the call names (`owner`, `repo`) and ask GitHub for
its visibility:

  github.repository-visibility   what a read returns: content written by
                                 whoever pushed it (`suspicious`), read by
                                 everyone for a public repository, by the
                                 repository's collaborators otherwise
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
A non-public repository's readers are the collection
`@github:repo/<owner>/<repo>/collaborators`, which the `github` audience
source resolves. The policy's mandate for each call names exactly that
spelling; the script refuses a consult whose mandate does not (exit
status 2) before it reads a token.

Credentials come from APPA_PROVIDER_GITHUB_TOKEN. A repository the token
cannot see, or any GitHub error, exits nonzero: the runtime treats that
as no answer and refuses the operation, so nothing is guessed public.
"""

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request


API_ROOT = "https://api.github.com"
TOKEN_VAR = "APPA_PROVIDER_GITHUB_TOKEN"
CONTENT = "github.repository-visibility"
READERS = "github.repository-readers"
TIMEOUT_SECONDS = 30
MAX_INPUT_BYTES = 64 * 1024


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
    """The annotator asked and the repository the call names."""
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
    return name, owner, repo


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


def repository_visibility(call, owner, repo):
    payload = call(f"/repos/{urllib.parse.quote(owner, safe='')}/{urllib.parse.quote(repo, safe='')}")
    visibility = payload.get("visibility") if isinstance(payload, dict) else None
    if visibility not in VISIBILITIES:
        raise RuntimeError(f"GitHub reports the unknown repository visibility {visibility!r}")
    return visibility


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


def annotation(name, visibility, owner, repo):
    match name:
        case "github.repository-visibility":
            return {
                "delta": {"trust": "suspicious", "audience": read_by(visibility, owner, repo)},
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
    name, owner, repo = repository_of(consult)
    check_declaration(consult, owner, repo)

    token = os.environ.get(TOKEN_VAR)
    if not token:
        raise RuntimeError(f"{TOKEN_VAR} is not set")

    visibility = repository_visibility(rest_api(token), owner, repo)
    json.dump({"version": 1, "answer": annotation(name, visibility, owner, repo)}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"github repository annotator: {error}", file=sys.stderr)
        raise SystemExit(1)
