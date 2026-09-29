"""The `github` context provider: facts about the repository, pull request,
or issue a proposed call reaches on GitHub.

One consult in (`kind = "context"`, the proposed call and the directory
the harness would run it in), one answer out:

  {"version": 1, "answer": null}      the call reaches nothing on GitHub
  {"version": 1, "answer": {"viewer": "ana", "repository": {...},
                            "pull_request" | "issue": {...}}}

Two kinds of call are recognized. A `Bash` call is split into its simple
commands; each `gh` call and `git push` names its repository by
`--repo`/`-R`, `GH_REPO`, a `repos/OWNER/NAME` API path, a GitHub URL, a
remote, or the checkout (read from the checkout's git config, never the
network), and a pull request or issue by its number or URL. A
`mcp/github/<tool>` call names it by `owner`, `repo`, and `pullNumber` or
`issue_number`. Every other call answers `null` before any network.

A recognized call costs one GraphQL query. The answer carries facts
only: who wrote the pull request or issue, every comment and review on
it, who edited them, and whether a connection had more pages than were
read (`truncated`). A command this provider cannot follow (a shell, a
subshell, a computed target, a setting that moves git), a repository the
token cannot see, or any GitHub error exits nonzero: the runtime hands
the annotator an error entry, and the call itself is never refused here.
"""

from dataclasses import dataclass
import json
import os
import re
import shlex
import subprocess
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from github_token import api_root, resolve_token  # noqa: E402

MAX_INPUT_BYTES = 4 * 1024 * 1024
TIMEOUT_SECONDS = 10
GIT_TIMEOUT_SECONDS = 4

OPERATORS = "();<>&|\n"
PREFIXES = {"sudo", "env", "command", "exec", "nohup", "time", "then", "do", "else", "!", "{"}
INTERPRETERS = {"bash", "sh", "zsh", "dash", "ksh", "mksh", "csh", "tcsh", "fish", "pwsh", "busybox", "eval", "source", ".", "xargs"}
GLOB = set("*?[")
SETTERS = {"declare", "typeset", "local", "readonly", "read", "mapfile", "readarray", "getopts", "alias", "unset"}
REMOTE_CHANGES = {"add", "set-url", "rename", "remove", "rm", "set-head"}
PUSH_OPTIONS_WITH_VALUE = {"-o", "--push-option", "--receive-pack", "--exec", "--repo"}
HARMLESS_GIT_OPTIONS = {"--no-pager", "--paginate", "-P", "--no-replace-objects"}
GIT_OPTIONS_WITH_VALUE = {"-c", "--git-dir", "--work-tree", "--namespace", "--config-env", "--exec-path"}
HARMLESS_VARIABLES = {"GH_REPO", "GH_PROMPT_DISABLED", "GH_PAGER", "GIT_PAGER", "PAGER", "GIT_TERMINAL_PROMPT", "NO_COLOR", "CI"}

GITHUB_URL = re.compile(r"^(?:https?://|git@|ssh://git@)github\.com[/:]([\w.-]+)/([\w.-]+?)(?:\.git)?(?:/(?:pull|issues)/(\d+))?(?:[/#?]|$)")
API_PATH = re.compile(r"^(?:https://api\.github\.com)?/?repos/([\w.-]+)/([\w.-]+)(?:/(?:issues|pulls)/(\d+))?")
CHECKOUT_API_PATH = re.compile(r"^/?repos/\{owner\}/\{repo\}(?:/(?:issues|pulls)/(\d+))?(?:/|$)")
URL = re.compile(r"^[a-z][a-z0-9+.-]*://", re.IGNORECASE)
SLUG = re.compile(r"^[\w.-]+/[\w.-]+$")
NUMBER = re.compile(r"^#?(\d+)$")
SUBSTITUTION = re.compile(r"\$\(([^)]*)|`([^`]*)", re.DOTALL)
INVOCATION = re.compile(r"\b(git|gh)\s")
MENTIONS_GIT = re.compile(r"\b(git|gh)\b")

# gh commands that act on one repository: `--repo`/`-R`, `GH_REPO`, or the checkout.
CHECKOUT_COMMANDS = {"api", "browse", "cache", "issue", "label", "pr", "release", "repo", "ruleset", "run", "secret", "variable", "workflow"}
# gh commands that reach no repository at all.
LOCAL_COMMANDS = {"auth", "config", "help", "version", "completion", "--version", "--help", "-h"}
# gh pr/issue flags that take the next word as their value.
GH_OPTIONS_WITH_VALUE = {
    "-R", "--repo", "--json", "-q", "--jq", "-t", "--template", "--title", "-b", "--body", "-F", "--body-file",
    "-B", "--base", "-H", "--head", "-a", "--assignee", "-l", "--label", "-m", "--milestone", "-p", "--project",
    "-r", "--reviewer", "-s", "--state", "-L", "--limit", "-A", "--author", "-S", "--search", "--add-label",
    "--remove-label", "--add-assignee", "--remove-assignee", "--add-reviewer", "--remove-reviewer",
    "--add-project", "--remove-project", "--subject", "--match-head-commit", "--reason",
    "-X", "--method", "-f", "--raw-field", "--field", "--input", "--header", "--cache", "--preview",
}
# MCP arguments naming a pull request or an issue.
NUMBER_ARGUMENTS = ("pullNumber", "pull_number", "issue_number", "issueNumber")
TRUNCATING_CONNECTIONS = 100


class Unfollowable(Exception):
    """The command reaches GitHub in a way this provider cannot establish."""


@dataclass(frozen=True)
class Target:
    """One place a call reaches: a `slug`, a `remote` of a checkout, the
    `checkout`'s own repository as gh resolves it, or the remote a bare
    `git push` goes to (`push`); with the pull request or issue it names."""

    kind: str
    value: str | None
    directory: str | None
    number: int | None = None


# ---------------------------------------------------------------- commands


def segments_of(command):
    """The command's simple commands, split at every shell operator and newline."""
    lexer = shlex.shlex(command, posix=True, punctuation_chars=OPERATORS)
    lexer.whitespace = " \t\r"
    lexer.whitespace_split = True
    segment, previous = [], ""
    try:
        for token in lexer:
            if token == "(" and not previous.endswith("$"):
                raise Unfollowable("a subshell's directory and settings do not reach the rest of the command")
            previous = token
            if set(token) <= set(OPERATORS):
                if segment:
                    yield segment
                segment = []
            else:
                segment.append(token)
    except ValueError as error:
        raise Unfollowable(f"the command does not parse: {error}") from None
    if segment:
        yield segment


def computed(word):
    if "$" in word or "`" in word:
        raise Unfollowable(f"{word} is computed when the command runs")
    return word


def within(directory, path):
    if path == "-" or {"$", "`", "~"} & set(path):
        raise Unfollowable(f"{path} is a directory this provider cannot follow")
    return os.path.join(directory, path) if directory else path


def url_target(word, directory):
    """A GitHub URL as a slug target, `None` for a URL on another host."""
    match = GITHUB_URL.match(computed(word))
    if not match:
        return None
    number = int(match.group(3)) if match.group(3) else None
    return Target("slug", f"{match.group(1)}/{match.group(2)}", directory, number)


def git_target(words, directory):
    """Where one `git` call pushes, or `None` for a call that pushes
    nothing on GitHub."""
    index, unfollowable = 0, None
    while index < len(words) and words[index].startswith("-"):
        option = words[index]
        if option.startswith("-C") and (option != "-C" or index + 1 < len(words)):
            if option == "-C":
                index += 1
            directory = within(directory, option.removeprefix("-C") or words[index])
        elif option not in HARMLESS_GIT_OPTIONS:
            unfollowable = option
            index += option in GIT_OPTIONS_WITH_VALUE
        index += 1
    match words[index : index + 2]:
        case ["config", *_]:
            raise Unfollowable("git config changes where a later push goes")
        case ["remote", change] if change in REMOTE_CHANGES:
            raise Unfollowable(f"git remote {change} changes where a later push goes")
        case ["push", *_]:
            pass
        case _:
            return None
    if unfollowable:
        raise Unfollowable(f"git {unfollowable} changes which repository a push reaches")

    def destination(word):
        if "://" in word or word.startswith("git@"):
            return url_target(word, directory)
        return Target("remote", computed(word), directory)

    # A positional repository wins over `--repo`, as in git itself.
    repo = None
    arguments = iter(words[index + 1 :])
    for word in arguments:
        if word == "--repo":
            repo = next(arguments, None)
        elif word.startswith("--repo="):
            repo = word.removeprefix("--repo=")
        elif word in PUSH_OPTIONS_WITH_VALUE:
            next(arguments, None)
        elif not word.startswith("-"):
            return destination(word)
    return destination(repo) if repo else Target("push", None, directory)


def positionals(words):
    """The words of a gh call that are neither options nor option values."""
    arguments = iter(words)
    for word in arguments:
        if word in GH_OPTIONS_WITH_VALUE:
            next(arguments, None)
        elif not word.startswith("-"):
            yield word


def gh_targets(words, environment, directory):
    """What one `gh` call reaches: a URL, API path, or `gh repo` slug names
    the repository, else `--repo`/`-R` or `GH_REPO`, else the checkout; a
    number or URL names the pull request or issue."""
    if not words or words[0] in LOCAL_COMMANDS:
        return []
    if words[0] not in CHECKOUT_COMMANDS:
        raise Unfollowable(f"gh {words[0]} reaches a destination other than a named or checked-out repository")
    for previous, word in zip(["gh", *words], words):
        if URL.match(word) and not (GITHUB_URL.match(word) or API_PATH.match(word)) and not previous.startswith("-"):
            raise Unfollowable(f"{word} is on a host other than github.com")
    chosen = environment.get("GH_REPO")
    arguments = iter(words)
    for word in arguments:
        if word.startswith("--hostname"):
            raise Unfollowable("gh --hostname reaches a host other than github.com")
        if word in ("--repo", "-R"):
            chosen = next(arguments, chosen)
        elif word.startswith(("--repo=", "-R")):
            chosen = word.removeprefix("--repo=").removeprefix("-R").removeprefix("=")
        elif word in GH_OPTIONS_WITH_VALUE:
            next(arguments, None)
    slug, number = None, None
    rest = list(positionals(words[1:]))
    match words[0], rest:
        case "repo", ["create" | "fork", *_]:
            raise Unfollowable(f"gh repo {rest[0]} creates a repository this provider cannot establish")
        case "repo", [_, named, *_]:
            slug = computed(named)
        case "api", [endpoint, *_]:
            if match := API_PATH.match(computed(endpoint)):
                slug, number = f"{match.group(1)}/{match.group(2)}", match.group(3)
            elif match := CHECKOUT_API_PATH.match(endpoint):
                number = match.group(1)
            else:
                raise Unfollowable(f"gh api {endpoint} names no repository this provider can establish")
        case "api", []:
            raise Unfollowable("gh api names no endpoint")
        case "pr" | "issue", [_, named, *_]:
            if match := GITHUB_URL.match(computed(named)):
                slug, number = f"{match.group(1)}/{match.group(2)}", match.group(3)
            elif match := NUMBER.match(named):
                number = match.group(1)
    if slug is not None and GITHUB_URL.match(slug):
        slug = url_target(slug, directory).value
    # A URL or path names its repository outright, as in gh itself.
    if slug is None and chosen is not None:
        slug = computed(chosen)
    if slug is not None and not SLUG.match(slug):
        raise Unfollowable(f"{slug} names no repository this provider can establish")
    number = int(number) if number else None
    return [Target("slug", slug, directory, number) if slug else Target("checkout", None, directory, number)]


def bash_targets(command, cwd):
    """Every place on GitHub the command reaches."""
    if any(INVOCATION.search("".join(inner)) for inner in SUBSTITUTION.findall(command)):
        raise Unfollowable("a command substitution runs a git or gh call this provider cannot follow")
    targets, exported, assigned, directory = [], {}, set(), cwd
    for words in segments_of(command):
        environment = dict(exported)
        exporting = words[0] == "export"
        if exporting:
            words = words[1:]
        while words and ("=" in words[0] and not words[0].startswith(("=", "-")) or words[0] in PREFIXES):
            name, assigns, value = words[0].partition("=")
            if assigns:
                environment[name] = value
            words = words[1:]
        if not words and exporting:
            exported = environment
            continue
        if exporting:
            raise Unfollowable(f"export {' '.join(words)} changes settings this provider cannot follow")
        if not words:
            assigned |= {name for name, value in environment.items() if exported.get(name) != value}
            continue
        program = os.path.basename(words[0])
        if program in ("git", "gh") and program != words[0]:
            raise Unfollowable(f"{words[0]} may not be the {program} this provider follows")
        if program in ("git", "gh") and (settings := assigned | environment.keys() - HARMLESS_VARIABLES):
            raise Unfollowable(f"{', '.join(sorted(settings))} may change which repository or login a call uses")
        match program:
            case "git":
                target = git_target(words[1:], directory)
                targets += [target] if target else []
            case "gh":
                targets += gh_targets(words[1:], environment, directory)
            case "cd" | "pushd" if len(words) == 2:
                directory = within(directory, words[1])
            case "cd" | "pushd" | "popd":
                raise Unfollowable(f"{' '.join(words)} moves to a directory this provider cannot follow")
            case _ if program in INTERPRETERS:
                raise Unfollowable(f"{program} runs commands this provider cannot follow")
            case _ if program in SETTERS or program == "printf" and any(word.startswith("-v") for word in words):
                raise Unfollowable(f"{program} sets a variable this provider cannot follow")
            case _ if (
                GLOB & set(program)
                or "$" in program
                or "`" in program
                or any(os.path.basename(word) in ("git", "gh") or INVOCATION.search(word) for word in words)
            ):
                raise Unfollowable(f"{program} runs a git or gh call this provider cannot follow")
    return list(dict.fromkeys(targets))


def mcp_target(tool, arguments):
    """The repository and pull request or issue a GitHub MCP call names."""
    owner, repo = arguments.get("owner"), arguments.get("repo")
    if not all(isinstance(value, str) and value and "/" not in value and not value.startswith("$") for value in (owner, repo)):
        return None
    numbers = [arguments[key] for key in NUMBER_ARGUMENTS if key in arguments]
    number = next((value for value in numbers if isinstance(value, int) and not isinstance(value, bool) and value > 0), None)
    return Target("slug", f"{owner}/{repo}", None, number)


def call_targets(artifact):
    """What the proposed call reaches, `[]` for a call that reaches nothing
    on GitHub."""
    tool = artifact.get("tool")
    arguments = artifact.get("arguments")
    if not isinstance(tool, str) or not isinstance(arguments, dict):
        return []
    if tool.startswith("mcp/github/"):
        target = mcp_target(tool, arguments)
        return [target] if target else []
    if tool.rsplit("/", 1)[-1] != "Bash":
        return []
    command = arguments.get("command")
    if not isinstance(command, str) or not MENTIONS_GIT.search(command):
        return []
    cwd = artifact.get("cwd")
    return bash_targets(command, cwd if isinstance(cwd, str) else None)


# ---------------------------------------------------------------- checkouts


def parse_git_config(listing):
    """`git config --list` output as a key → value map; the last value wins."""
    config = {}
    for line in listing.splitlines():
        key, _, value = line.partition("=")
        config[key.lower() if key.count(".") < 2 else key] = value
    return config


def remotes_of(config):
    """Every remote with a URL, by name."""
    remotes = {}
    for key, value in config.items():
        match key.split("."):
            case ["remote", *name, variable] if name and variable.lower() == "url":
                remotes[".".join(name)] = value
    return remotes


def slug_of_url(url):
    match = GITHUB_URL.match(url or "")
    return f"{match.group(1)}/{match.group(2)}" if match else None


def remote_repository(config, remote):
    """The GitHub repository a remote pushes to, `None` for another host."""
    url = config.get(f"remote.{remote}.pushurl") or config.get(f"remote.{remote}.url")
    if url is None:
        raise Unfollowable(f"remote {remote} is not configured here")
    return slug_of_url(url)


def checkout_repository(config):
    """The repository gh picks for the checkout: the remote `gh repo
    set-default` marked, else the one GitHub repository the remotes name."""
    remotes = remotes_of(config)
    for name in remotes:
        resolved = config.get(f"remote.{name}.gh-resolved")
        match resolved:
            case "base":
                return slug_of_url(remotes[name])
            case str() if SLUG.match(resolved):
                return resolved
    slugs = {slug for slug in map(slug_of_url, remotes.values()) if slug}
    match sorted(slugs):
        case [slug]:
            return slug
        case []:
            raise Unfollowable("the checkout has no GitHub remote")
        case _:
            raise Unfollowable("the checkout's remotes name several repositories and gh has no default set")


def push_repository(config, branch):
    """Where a bare `git push` goes: the branch's push remote, else the
    default push remote, else the branch's remote, else `origin`."""
    remote = (
        (branch and config.get(f"branch.{branch}.pushremote"))
        or config.get("remote.pushdefault")
        or (branch and config.get(f"branch.{branch}.remote"))
        or "origin"
    )
    return remote_repository(config, remote)


def git(directory, *arguments):
    completed = subprocess.run(
        ["git", "-C", directory, *arguments],
        capture_output=True,
        text=True,
        timeout=GIT_TIMEOUT_SECONDS,
        check=False,
    )
    return completed.stdout if completed.returncode == 0 else None


def read_checkout(directory):
    """The checkout's git config and current branch, read locally."""
    if not (directory and os.path.isabs(directory) and os.path.isdir(directory)):
        raise Unfollowable("the command reaches a checkout in a directory this provider cannot read")
    listing = git(directory, "config", "--list")
    if listing is None:
        raise Unfollowable(f"{directory} is not a git checkout")
    branch = (git(directory, "symbolic-ref", "--short", "-q", "HEAD") or "").strip() or None
    return parse_git_config(listing), branch


def repository_of(target):
    """The `owner/name` a target reaches, `None` for a host other than GitHub."""
    match target.kind:
        case "slug":
            return target.value
        case "remote":
            return remote_repository(read_checkout(target.directory)[0], target.value)
        case "checkout":
            return checkout_repository(read_checkout(target.directory)[0])
        case "push":
            return push_repository(*read_checkout(target.directory))
    raise ValueError(f"unexpected target kind {target.kind!r}")


def reached(targets):
    """The one repository and at most one pull request or issue the call
    reaches, or `None` when it reaches nothing on GitHub."""
    places = {(repository_of(target), target.number) for target in targets}
    places = {(slug, number) for slug, number in places if slug}
    slugs = {slug.lower() for slug, _ in places}
    numbers = {number for _, number in places if number}
    if not places:
        return None
    if len(slugs) > 1:
        raise Unfollowable(f"the command reaches {len(slugs)} repositories")
    if len(numbers) > 1:
        raise Unfollowable(f"the command reaches {len(numbers)} pull requests or issues")
    slug = sorted(places)[0][0]
    return slug, next(iter(numbers), None)


# ---------------------------------------------------------------- GitHub

ACTOR = "author { login __typename } authorAssociation editor { login } lastEditedAt"
QUERY = f"""
query($owner: String!, $name: String!, $number: Int!, $withNumber: Boolean!) {{
  viewer {{ login }}
  repository(owner: $owner, name: $name) {{
    nameWithOwner visibility viewerPermission parent {{ nameWithOwner }}
    issueOrPullRequest(number: $number) @include(if: $withNumber) {{
      __typename
      ... on Issue {{
        number locked {ACTOR}
        comments(first: 100) {{ pageInfo {{ hasNextPage }} nodes {{ {ACTOR} }} }}
      }}
      ... on PullRequest {{
        number locked isCrossRepository {ACTOR}
        comments(first: 100) {{ pageInfo {{ hasNextPage }} nodes {{ {ACTOR} }} }}
        reviews(first: 50) {{ pageInfo {{ hasNextPage }} nodes {{
          {ACTOR}
          comments(first: 30) {{ pageInfo {{ hasNextPage }} nodes {{ {ACTOR} }} }}
        }} }}
        commits(first: 100) {{ pageInfo {{ hasNextPage }} nodes {{ commit {{
          authors(first: 10) {{ pageInfo {{ hasNextPage }} nodes {{ user {{ login }} }} }}
        }} }} }}
      }}
    }}
  }}
}}
"""


def variables_of(slug, number):
    owner, name = slug.split("/")
    return {"owner": owner, "name": name, "number": number or 0, "withNumber": number is not None}


def graphql_url(root):
    """GitHub's GraphQL endpoint beside a REST root: `/api/graphql` for an
    Enterprise Server's `/api/v3`, `/graphql` otherwise."""
    return root.removesuffix("/v3") + "/graphql" if root.endswith("/api/v3") else f"{root}/graphql"


def graphql(token, variables):
    request = urllib.request.Request(
        graphql_url(api_root(os.environ)),
        data=json.dumps({"query": QUERY, "variables": variables}).encode(),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"GitHub GraphQL failed: {error.code}") from error


def connection(node, name):
    """A connection's nodes and whether it had more pages than were read."""
    found = node.get(name) or {}
    return found.get("nodes") or [], bool((found.get("pageInfo") or {}).get("hasNextPage"))


def item_of(node):
    """The facts about one pull request or issue from its GraphQL node."""
    written = [node]
    comments, truncated = connection(node, "comments")
    written += comments
    reviews, more = connection(node, "reviews")
    truncated |= more
    for review in reviews:
        written.append(review)
        review_comments, more = connection(review, "comments")
        written += review_comments
        truncated |= more

    participants, seen, editors = [], set(), []
    for text in written:
        author = text.get("author") or {}
        login = author.get("login")
        if login not in seen:
            seen.add(login)
            participants.append(
                {"login": login, "association": text.get("authorAssociation"), "bot": author.get("__typename") == "Bot"}
            )
        if text.get("lastEditedAt"):
            editor = (text.get("editor") or {}).get("login")
            if editor not in editors:
                editors.append(editor)

    author = node.get("author") or {}
    item = {
        "number": node.get("number"),
        "author": {"login": author.get("login"), "association": node.get("authorAssociation")},
        "locked": bool(node.get("locked")),
    }
    if node.get("__typename") == "PullRequest":
        item["cross_repository"] = bool(node.get("isCrossRepository"))
    item["participants"] = participants
    if node.get("__typename") == "PullRequest":
        commits, more = connection(node, "commits")
        truncated |= more
        commit_authors = []
        for commit in commits:
            authors, more = connection(commit.get("commit") or {}, "authors")
            truncated |= more
            for git_author in authors:
                login = (git_author.get("user") or {}).get("login")
                if login not in commit_authors:
                    commit_authors.append(login)
        item["commit_authors"] = commit_authors
    item["last_editors"] = editors
    item["truncated"] = truncated
    return item


def answer_of(payload):
    """The provider's answer from one GraphQL response."""
    data = payload.get("data") if isinstance(payload, dict) else None
    repository = data.get("repository") if isinstance(data, dict) else None
    errors = payload.get("errors") or [] if isinstance(payload, dict) else []
    # A number that names nothing leaves the item out and the repository in.
    unexpected = [error for error in errors if (error.get("path") or [])[-1:] != ["issueOrPullRequest"]]
    if repository is None or unexpected:
        messages = "; ".join(str(error.get("message")) for error in errors) or "no repository in the response"
        raise RuntimeError(f"GitHub GraphQL: {messages}")
    visibility = str(repository.get("visibility") or "").lower()
    if visibility not in ("public", "private", "internal"):
        raise RuntimeError(f"GitHub reports the unknown repository visibility {visibility!r}")
    answer = {
        "viewer": (data.get("viewer") or {}).get("login"),
        "repository": {
            "name": repository.get("nameWithOwner"),
            "visibility": visibility,
            "viewer_permission": repository.get("viewerPermission"),
            "fork_of": (repository.get("parent") or {}).get("nameWithOwner"),
        },
    }
    node = repository.get("issueOrPullRequest")
    match (node or {}).get("__typename"):
        case "PullRequest":
            answer["pull_request"] = item_of(node)
        case "Issue":
            answer["issue"] = item_of(node)
    return answer


def context_of(consult):
    """The answer for one consult: `None` for a call that reaches nothing on
    GitHub, else the facts one GraphQL query returns."""
    if not isinstance(consult, dict) or consult.get("version") != 1 or consult.get("kind") != "context":
        raise ValueError("the consult must be a version 1 context consult")
    artifact = consult.get("artifact")
    if not isinstance(artifact, dict):
        raise ValueError("the consult carries no artifact")
    place = reached(call_targets(artifact))
    if place is None:
        return None
    return answer_of(graphql(resolve_token(), variables_of(*place)))


def main():
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        raise ValueError("the consult is too large")
    json.dump({"version": 1, "answer": context_of(json.loads(raw))}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"github context: {error}", file=sys.stderr)
        raise SystemExit(1)
