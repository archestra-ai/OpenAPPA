"""The `github` context provider: facts about the repository, pull request,
or issue a proposed call reaches on GitHub.

One consult in (`kind = "context"`, the proposed call and the directory
the harness would run it in), one answer out:

  {"version": 1, "answer": null}      the call reaches nothing on GitHub
  {"version": 1, "answer": {"viewer": "ana", "repository": {...},
                            "pull_request" | "issue": {...},
                            "issues" | "pull_requests": {...}}}

A multi-user deployment runs `context-multi-user.py` under a GitHub App
installation token, which has no viewer: its answer has no `viewer` and
no `viewer_permission`, names every person `"person": "github:<id>"`
where this one names its `"login"`, and reads the token from
APPA_PROVIDER_GITHUB_TOKEN alone. Its answer depends on no one's login, so
a call that sets its own GH_TOKEN or GITHUB_TOKEN is followed there.

Two kinds of call are recognized. A `Bash` call is split into its simple
commands; each `gh` call and `git push` names its repository by
`--repo`/`-R`, `GH_REPO`, a `repos/OWNER/NAME` API path, a GitHub URL, a
remote, or the checkout (read from the checkout's git config, never the
network), and a pull request or issue by its number or URL. A
`mcp/github/<tool>` call names it by `owner`, `repo`, and `pullNumber` or
`issue_number`. Every other call answers `null` before any network.

A listing (`gh issue list`, `gh pr list`, `list_issues`,
`list_pull_requests`) is repeated through GraphQL with the call's
filters: `"issues"` or `"pull_requests"` carries `items`, which hold
every item the call itself returns (possibly more), each with its author
and last editor, and `truncated`, which is `true` whenever that cannot be
established: a search, a flag or argument this provider does not know,
a page it cannot reach, or a superset of the call's filters that has
more items than one query reads.

A recognized call costs one GraphQL query. The answer carries facts
only: who wrote the pull request or issue, every comment and review on
it, who edited them, and whether a connection had more pages than were
read (`truncated`). A command this provider cannot follow (a shell, a
computed target, a setting that moves git), a repository the
token cannot see, or any GitHub error exits nonzero: the runtime hands
the annotator an error entry, and the call itself is never refused here.
Every GitHub and git wait ends inside APPA_CONSULT_TIMEOUT_MS, the budget
the runtime gives the consult, when it sets one.
"""

from dataclasses import dataclass
from enum import Enum
import json
import os
import re
import shlex
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from github_token import GH_TIMEOUT_SECONDS, api_root, env_token, resolve_token  # noqa: E402

MAX_INPUT_BYTES = 4 * 1024 * 1024
TIMEOUT_SECONDS = 10
GIT_TIMEOUT_SECONDS = 4
CONSULT_TIMEOUT = "APPA_CONSULT_TIMEOUT_MS"
# What is kept of the consult's budget for the answer to reach the runtime.
# The clock starts after interpreter start-up, which the margin also covers.
ANSWER_MARGIN_SECONDS = 0.5
STARTED = time.monotonic()

OPERATORS = "();<>&|\n"
PREFIXES = {"sudo", "env", "command", "exec", "nohup", "time", "if", "elif", "while", "until", "then", "do", "else", "!", "{"}
# Programs that run the command after them as it is: their options that take a value, and
# how many operands stand before that command.
WRAPPERS = {"timeout": ({"-s", "--signal", "-k", "--kill-after"}, 1), "nice": ({"-n", "--adjustment"}, 0)}
INTERPRETERS = {"bash", "sh", "zsh", "dash", "ksh", "mksh", "csh", "tcsh", "fish", "pwsh", "busybox", "eval", "source", ".", "xargs"}
GLOB = set("*?[")
SETTERS = {"declare", "typeset", "local", "readonly", "read", "mapfile", "readarray", "getopts", "alias", "unset"}
REMOTE_CHANGES = {"add", "set-url", "rename", "remove", "rm", "set-head"}
PUSH_OPTIONS_WITH_VALUE = {"-o", "--push-option", "--receive-pack", "--exec", "--repo"}
HARMLESS_GIT_OPTIONS = {"--no-pager", "--paginate", "-P", "--no-replace-objects"}
GIT_OPTIONS_WITH_VALUE = {"-c", "--git-dir", "--work-tree", "--namespace", "--config-env", "--exec-path"}
HARMLESS_VARIABLES = {"GH_REPO", "GH_PROMPT_DISABLED", "GH_PAGER", "GIT_PAGER", "PAGER", "GIT_TERMINAL_PROMPT", "NO_COLOR", "CI"}
TOKEN_VARIABLES = {"GH_TOKEN", "GITHUB_TOKEN"}
# Shell variables that git, gh, or the shell itself read, as they are or once exported.
READ_PREFIXES = ("GIT_", "GH_", "GITHUB_", "XDG_", "LD_", "DYLD_")
READ_VARIABLES = {
    "HOME", "PATH", "CDPATH", "PS4", "PROMPT_COMMAND", "BASH_ENV", "ENV", "http_proxy", "https_proxy", "all_proxy",
    "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "SSL_CERT_FILE", "SSL_CERT_DIR", "CURL_CA_BUNDLE",
}
# git config sections and variables that change which repository or host a push or the
# checkout's remotes reach (git-config(1), git-push(1)), or run commands in a push's place.
MOVING_SECTIONS = {"remote", "url", "push", "branch", "include", "includeif", "http"}
MOVING_VARIABLES = {
    ("core", "sshcommand"), ("core", "gitproxy"), ("core", "hookspath"), ("credential", "url"), ("extensions", "worktreeconfig"),
}
CONFIG_READS = {"--get", "--get-all", "--get-regexp", "--get-urlmatch", "-l", "--list", "--get-color", "--get-colorbool"}
CONFIG_WRITES = {"--add", "--replace-all", "--unset", "--unset-all", "--rename-section", "--remove-section"}
CONFIG_SWITCHES = {
    "--global", "--system", "--local", "--worktree", "--includes", "--no-includes", "--show-origin", "--show-scope",
    "--name-only", "-z", "--null", "--bool", "--int", "--bool-or-int", "--path", "--expiry-date", "--fixed-value", "--all",
}
CONFIG_OPTIONS_WITH_VALUE = {"-f", "--file", "--blob", "--type", "--default", "--comment", "--value", "--url"}

GITHUB_URL = re.compile(r"^(?:https?://|git@|ssh://git@)github\.com[/:]([\w.-]+)/([\w.-]+?)(?:\.git)?(?:/(?:pull|issues)/(\d+))?(?:[/#?]|$)")
API_PATH = re.compile(r"^(?:https://api\.github\.com)?/?repos/([\w.-]+)/([\w.-]+)(?:/(?:issues|pulls)/(\d+))?")
CHECKOUT_API_PATH = re.compile(r"^/?repos/\{owner\}/\{repo\}(?:/(?:issues|pulls)/(\d+))?(?:/|$)")
URL = re.compile(r"^[a-z][a-z0-9+.-]*://", re.IGNORECASE)
SLUG = re.compile(r"^[\w.-]+/[\w.-]+$")
NUMBER = re.compile(r"^#?(\d+)$")
BACKTICKS = re.compile(r"`([^`]*)")
INVOCATION = re.compile(r"\b(git|gh)\s")
MENTIONS_GIT = re.compile(r"\b(git|gh)\b")
QUOTING = re.compile(r"\\\n|[\"'\\]")
RUNS_GIT = re.compile(r"(?<![\w.-])(git|gh)(?![\w@.:/-])")
# Text that names a file or directory git reads its settings or hooks from.
GIT_FILE = re.compile(r"(?<![\w-])\.git(?![\w-])|\.gitconfig\b|\bgit/config\b")
REFERENCE = re.compile(r"\$\{?[#!]?([A-Za-z_]\w*)")
HEREDOC = re.compile(r"<<(-?)[ \t]*")

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
    "--mention", "--app", "--type",
}
# `gh issue list` and `gh pr list`: short flags, and the flags each accepts.
LIST_SHORT = {
    "-s": "--state", "-l": "--label", "-L": "--limit", "-A": "--author", "-a": "--assignee", "-m": "--milestone",
    "-S": "--search", "-B": "--base", "-H": "--head", "-q": "--jq", "-t": "--template", "-R": "--repo",
    "-w": "--web", "-d": "--draft",
}
OUTPUT_FLAGS = {"--json", "--jq", "--template", "--repo"}
LIST_VALUE_FLAGS = {
    "issue": {"--state", "--label", "--limit", "--author", "--assignee", "--mention", "--milestone", "--type", "--app", "--search"} | OUTPUT_FLAGS,
    "pr": {"--state", "--label", "--limit", "--author", "--assignee", "--app", "--base", "--head", "--search"} | OUTPUT_FLAGS,
}
LIST_SWITCHES = {"issue": {"--web"}, "pr": {"--web", "--draft"}}
GH_LIST_STATES = {
    "issue": {"open": ("OPEN",), "closed": ("CLOSED",), "all": ("OPEN", "CLOSED")},
    "pr": {"open": ("OPEN",), "closed": ("CLOSED", "MERGED"), "merged": ("MERGED",), "all": ("OPEN", "CLOSED", "MERGED")},
}
REST_PULL_REQUEST_STATES = {"open": ("OPEN",), "closed": ("CLOSED", "MERGED"), "all": ("OPEN", "CLOSED", "MERGED")}
REST_SORTS = {"created": "CREATED_AT", "updated": "UPDATED_AT", "popularity": None, "long-running": None}
ISSUE_ORDERS = {"CREATED_AT", "UPDATED_AT", "COMMENTS"}
DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
DATE_TIME = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$")
# MCP arguments naming a pull request or an issue.
NUMBER_ARGUMENTS = ("pullNumber", "pull_number", "issue_number", "issueNumber")


class Unfollowable(Exception):
    """The command reaches GitHub in a way this provider cannot establish."""


def login_of(account):
    return account.get("login")


def github_id_of(account):
    """`github:<databaseId>` for a user or a bot; `None` for a deleted
    account, a mannequin, or any other actor the query reads no id for."""
    match account.get("databaseId"):
        case int() as number if not isinstance(number, bool) and number > 0:
            return f"github:{number}"
        case _:
            return None


class Deployment(Enum):
    """Whose token the provider holds, which decides how the answer names a
    person and where the token comes from.

    A single-user deployment's token is a person's: the answer names the
    viewer, the viewer's permission, and every person by `login`. A
    multi-user deployment's token is a GitHub App installation's, which
    has no viewer: every person is `person`, `github:<id>`, and the token
    comes from the deployment alone."""

    SINGLE_USER = "single-user"
    MULTI_USER = "multi-user"

    @property
    def has_viewer(self):
        return self is Deployment.SINGLE_USER

    @property
    def key(self):
        match self:
            case Deployment.SINGLE_USER:
                return "login"
            case Deployment.MULTI_USER:
                return "person"

    def person(self, account):
        match self:
            case Deployment.SINGLE_USER:
                return login_of(account)
            case Deployment.MULTI_USER:
                return github_id_of(account)

    def token(self):
        match self:
            case Deployment.SINGLE_USER:
                return resolve_token(timeout=within_budget(GH_TIMEOUT_SECONDS))
            case Deployment.MULTI_USER:
                return env_token()

    @property
    def harmless_variables(self):
        """Variables a git or gh call may carry without changing a fact the answer reports.
        The repository comes from the command or the checkout, never from the token, so the
        call's own token matters only to an answer that names the viewer."""
        match self:
            case Deployment.SINGLE_USER:
                return HARMLESS_VARIABLES
            case Deployment.MULTI_USER:
                return HARMLESS_VARIABLES | TOKEN_VARIABLES


# How faithfully a listing's query repeats the call:
PREFIX = "prefix"  # same filters and order: the first `first` items cover the call
WHOLE = "whole"  # filters that admit every item the call returns, in another order: covered only when no page follows
UNKNOWN = "unknown"  # the call cannot be repeated; nothing is fetched
MAX_FIRST = 100


@dataclass(frozen=True)
class Listing:
    """A listing call repeated as one GraphQL connection read."""

    kind: str
    scope: str
    first: int = 0
    states: tuple[str, ...] = ()
    labels: tuple[str, ...] = ()
    order: tuple[str, str] = ("CREATED_AT", "DESC")
    filters: tuple[tuple[str, str], ...] = ()
    after: str | None = None
    base: str | None = None
    head: str | None = None


def listing(kind, prefix, first, **fields):
    """A PREFIX listing when the query repeats the call's filters and order
    and one page reaches every item the call returns, WHOLE otherwise."""
    if prefix and first <= MAX_FIRST:
        return Listing(kind, PREFIX, first, **fields)
    return Listing(kind, WHOLE, MAX_FIRST, **fields)


@dataclass(frozen=True)
class Target:
    """One place a call reaches: a `slug`, a `remote` of a checkout, the
    `checkout`'s own repository as gh resolves it, or the remote a bare
    `git push` goes to (`push`); with the pull request or issue, or the
    listing, it names."""

    kind: str
    value: str | None
    directory: str | None
    number: int | None = None
    listing: Listing | None = None


# ---------------------------------------------------------------- commands


def names_git(text):
    """Whether text names git or gh, as it is or once the shell removes its quotes."""
    return bool(MENTIONS_GIT.search(text) or MENTIONS_GIT.search(QUOTING.sub("", text)))


def runs_git(value):
    """Whether a setting's value runs git or gh: the program named as a command, not
    inside a URL such as `git@github.com:` or `acme/api.git`."""
    return bool(RUNS_GIT.search(QUOTING.sub("", value)))


def segments_of(command):
    """The command's simple commands, split at every shell operator and newline, with
    `(` and `)` where a subshell or an unquoted command substitution opens and closes.
    An array's values (`name=(...)`) are left out of its assignment."""
    lexer = shlex.shlex(command, posix=True, punctuation_chars=OPERATORS)
    lexer.whitespace = " \t\r"
    lexer.whitespace_split = True
    segment, array, previous, redirected = [], False, "", False
    try:
        for token in lexer:
            if not set(token) <= set(OPERATORS):
                if "$(" in token and (array or names_git(token)):
                    raise Unfollowable("a quoted command substitution runs a git or gh call this provider cannot follow")
                # A redirection's file or here-document delimiter is no word of the command.
                if not array and not redirected:
                    segment.append(token)
                previous, redirected = token, False
                continue
            if not array and set(token) <= set("<>&") and set(token) & set("<>"):
                redirected = True
                continue
            redirected = False
            if "()" in token and segment and not array and not segment[-1].endswith("="):
                raise Unfollowable(f"{segment[-1]}() defines a function whose calls this provider cannot follow")
            for character in token:
                match character:
                    case ")" if array:
                        array = False
                    case _ if array:
                        raise Unfollowable("an array's values run commands this provider cannot follow")
                    case "(" if segment and segment[-1].endswith("="):
                        array = True
                    # Bash has no empty subshell: `( )` can only define a function.
                    case ")" if previous == "(":
                        raise Unfollowable("( ) defines a function whose calls this provider cannot follow")
                    case _:
                        if segment:
                            yield segment
                        segment = []
                        if character in "()":
                            yield character
                previous = character
    except ValueError as error:
        raise Unfollowable(f"the command does not parse: {error}") from None
    if array:
        raise Unfollowable("the command does not parse: an array is never closed")
    if segment:
        yield segment


def unwrapped(words):
    """The command a `timeout`- or `nice`-style wrapper runs; the words as they are for
    any other program."""
    while words and words[0] in WRAPPERS:
        with_value, operands = WRAPPERS[words[0]]
        index = 1
        while index < len(words) and words[index].startswith("-"):
            index += 1 + (words[index] in with_value)
        words = words[index + operands :]
    return words


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


@dataclass(frozen=True)
class Moves:
    """A `git config` or `git remote` call that changes where a push, or the
    checkout's own repository, goes."""

    change: str


def moving(name):
    """Whether a git config key or section changes where a push or the checkout's remotes go."""
    section, _, rest = name.partition(".")
    section, variable = section.lower(), rest.rpartition(".")[2].lower()
    if not rest:
        return section in MOVING_SECTIONS | {moved for moved, _ in MOVING_VARIABLES}
    return section in MOVING_SECTIONS or (section, variable) in MOVING_VARIABLES


def config_change(arguments):
    """What one `git config` call changes: `None` for a read or for settings that move
    no push, such as a credential helper or `user.*`."""
    names, mode, words = [], None, iter(arguments)
    for word in words:
        match word:
            case _ if "$" in word or "`" in word:
                raise Unfollowable(f"git config {word} is computed when the command runs")
            # Options end at the first operand: a later `--get` is a value pattern.
            case _ if names:
                names.append(word)
            case "-e" | "--edit" | "edit":
                raise Unfollowable("git config --edit changes settings this provider cannot read")
            case "get" | "list" if not names and mode is None:
                mode = "read"
            case "set" | "unset" | "rename-section" | "remove-section" if not names and mode is None:
                mode = "write"
            case _ if word in CONFIG_READS:
                mode = mode or "read"
            case _ if word in CONFIG_WRITES:
                mode = "write"
            case _ if word in CONFIG_SWITCHES or word.partition("=")[0] in CONFIG_OPTIONS_WITH_VALUE and "=" in word:
                pass
            case _ if word in CONFIG_OPTIONS_WITH_VALUE:
                next(words, None)
            case _ if word.startswith("-"):
                raise Unfollowable(f"git config {word} is an option this provider cannot read")
            case _:
                names.append(word)
    if mode == "read" or mode is None and len(names) < 2:
        return None
    # A helper, pager, or hook setting runs its value in a later call.
    if any(runs_git(name) for name in names):
        raise Unfollowable("git config sets a value that runs a git or gh call this provider cannot follow")
    if any(name.partition(".")[0].lower() == "alias" for name in names):
        raise Unfollowable("git config alias.* defines a git command this provider cannot follow")
    moved = next((name for name in names if moving(name)), None)
    return Moves(f"git config {moved}") if moved else None


def git_target(words, directory):
    """Where one `git` call pushes, a `Moves` for a call that changes where
    a push goes, or `None` for a call that pushes nothing on GitHub."""
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
            # A setting such as core.pager runs its value in the call it applies to.
            value = words[index] if index < len(words) else option
            if option.startswith("--config-env") or value.lower().startswith("alias.") or runs_git(value) or "$" in value or "`" in value:
                raise Unfollowable(f"git {option} {value} sets what the call runs, which this provider cannot follow")
        index += 1
    if index < len(words):
        computed(words[index])
    if any("ext::" in word for word in words):
        raise Unfollowable("git's ext:: transport runs a command this provider cannot follow")
    match words[index : index + 2]:
        case ["config", *_]:
            return config_change(words[index + 1 :])
        case ["remote", change] if change in REMOTE_CHANGES:
            return Moves(f"git remote {change}")
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


def list_flags(command, words):
    """The flags of a `gh issue|pr list` call as (name, value) pairs, `None`
    for a word gh does not accept there or this provider cannot read."""
    flags, listed = [], False
    arguments = iter(words)
    for word in arguments:
        if word in ("list", "ls") and not listed:
            listed = True
            continue
        if not word.startswith("-") or word in ("-", "--"):
            return None
        if word.startswith("--"):
            name, sign, value = word.partition("=")
        else:
            name, sign, value = word[:2], word[2:3], word[2:].removeprefix("=")
        name = LIST_SHORT.get(name, name)
        if name in LIST_SWITCHES[command]:
            if sign and value not in ("true", "false"):
                return None
            flags.append((name, value if sign else "true"))
        elif name in LIST_VALUE_FLAGS[command]:
            value = value if sign else next(arguments, None)
            if value is None or "$" in value or "`" in value:
                return None
            flags.append((name, value))
        else:
            return None
    return flags


def gh_listing(command, words):
    """The listing a `gh issue list` or `gh pr list` call reads, `None` for
    one that opens a browser instead. gh lists by creation, newest first,
    through GraphQL unless a filter sends it to the search API."""
    kind = "issues" if command == "issue" else "pull_requests"
    flags = list_flags(command, words)
    if flags is None:
        return Listing(kind, UNKNOWN)
    values = dict(flags)
    # A label flag repeats and splits at commas; gh wants every label, GraphQL any.
    labels = tuple(label for name, value in flags if name == "--label" for label in value.split(",") if label)
    if values.get("--web", "false") != "false":
        return None
    states = GH_LIST_STATES[command].get(values.get("--state", "open").lower())
    limit = values.get("--limit", "30")
    if "--search" in values or states is None or not limit.isdigit() or int(limit) < 1:
        return Listing(kind, UNKNOWN)
    match command:
        case "issue":
            people = {"createdBy": values.get("--author"), "assignee": values.get("--assignee"), "mentioned": values.get("--mention")}
            # `@me` is the viewer, known to this query only in its answer: the filter is dropped.
            kept = tuple((name, value) for name, value in people.items() if value and value != "@me")
            dropped = len(kept) < sum(bool(value) for value in people.values())
            searched = labels or {"--milestone", "--type", "--app"} & values.keys()
            return listing(kind, not (searched or dropped), int(limit), states=states, labels=labels, filters=kept)
        case "pr":
            searched = labels or {"--author", "--assignee", "--app", "--draft"} & values.keys()
            base, head = values.get("--base") or None, values.get("--head") or None
            return listing(kind, not searched, int(limit), states=states, labels=labels, base=base, head=head)
    raise ValueError(f"unexpected gh command {command!r}")


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
    slug, number, listed = None, None, None
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
        case "pr" | "issue", ["list" | "ls", *_]:
            listed = gh_listing(words[0], words[1:])
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
    return [Target("slug" if slug else "checkout", slug, directory, number, listed)]


def body_of(command, index, delimiter, tabs):
    """A here-document's body from `index`, and where its delimiter line ends; an
    unterminated body runs to the end, as in bash."""
    lines = []
    while index < len(command):
        end = command.find("\n", index)
        end = len(command) if end < 0 else end
        line, index = command[index:end], end + 1
        if (line.lstrip("\t") if tabs else line) == delimiter:
            break
        lines.append(line)
    return "\n".join(lines), index


def delimiter_at(command, index):
    """A here-document's delimiter word at `index`, as bash compares body lines with it
    after quote removal; where the word ends; and whether any of it was quoted."""
    delimiter, quoted, start = [], False, index
    while index < len(command) and command[index] not in " \t\n;&|<>()":
        match command[index]:
            case "'":
                end = command.find("'", index + 1)
                if end < 0:
                    raise Unfollowable("the command does not parse: a here-document delimiter is never closed")
                delimiter.append(command[index + 1 : end])
                index, quoted = end + 1, True
            case '"':
                index, quoted = index + 1, True
                while index < len(command) and command[index] != '"':
                    if command[index] == "\\" and command[index + 1 : index + 2] in ('"', "\\", "$", "`"):
                        index += 1
                    delimiter.append(command[index : index + 1])
                    index += 1
                if index >= len(command):
                    raise Unfollowable("the command does not parse: a here-document delimiter is never closed")
                index += 1
            case "$" if command[index + 1 : index + 2] in ("'", '"'):
                raise Unfollowable("a here-document delimiter quoted with $'...' or $\"...\" is one this provider cannot read")
            case "\\":
                delimiter.append(command[index + 1 : index + 2])
                index, quoted = index + 2, True
            case character:
                delimiter.append(character)
                index += 1
    if index == start:
        raise Unfollowable("the command does not parse: a here-document has no delimiter")
    return "".join(delimiter), index, quoted


def without_heredocs(command):
    """The command with each here-document's body left out: it is text a command reads,
    not shell. A body naming git or gh is refused, as the command reading it may run it."""
    kept, pending, quote, arithmetic, braces, index = [], [], None, 0, 0, 0
    while index < len(command):
        character = command[index]
        # A substitution parses its own here-documents before the pending ones.
        if pending and (command.startswith("$(", index) or character == "`"):
            raise Unfollowable("a command substitution beside a here-document is one this provider cannot follow")
        match character:
            case "\\" if quote != "'":
                kept.append(command[index : index + 2])
                index += 2
                continue
            case "'" | '"' if quote in (None, character):
                quote = None if quote else character
            case _ if quote:
                pass
            case "#" if not kept or kept[-1][-1] in " \t\n;&|()":
                end = command.find("\n", index)
                index = len(command) if end < 0 else end
                continue
            case "(" | ")" if command.startswith(character * 2, index) and (character == "(" or arithmetic):
                arithmetic += 1 if character == "(" else -1
                kept.append(character * 2)
                index += 2
                continue
            case "$" if command.startswith("${", index):
                braces += 1
                kept.append("${")
                index += 2
                continue
            case "}" if braces:
                braces -= 1
            case "<" if command.startswith("<<<", index):
                kept.append("<<<")
                index += 3
                continue
            case "<" if braces and command.startswith("<<", index):
                raise Unfollowable("<< inside a parameter expansion is one this provider cannot follow")
            case "<" if not arithmetic and (heredoc := HEREDOC.match(command, index)):
                delimiter, end, literal = delimiter_at(command, heredoc.end())
                pending.append((delimiter, bool(heredoc.group(1)), literal))
                kept.append(command[index:end])
                index = end
                continue
            case "\n" if pending:
                kept.append(character)
                index += 1
                for delimiter, tabs, literal in pending:
                    body, index = body_of(command, index, delimiter, tabs)
                    if names_git(body):
                        raise Unfollowable("a here-document hands a command text naming git or gh, which it may run")
                    # An unquoted delimiter leaves the body's substitutions and line joins to the shell.
                    if not literal and ("$(" in body or "`" in body or "\\\n" in f"{body}\n"):
                        raise Unfollowable("a here-document's body is expanded in a way this provider cannot follow")
                pending = []
                continue
        kept.append(character)
        index += 1
    return "".join(kept)


def unsafe_settings(words, environment, assigned, deployment):
    """Refuses a git or gh call whose environment, or a shell variable the command set,
    may change what the answer reports. A variable git and gh do not read matters only
    where the call expands it, which this provider does not do."""
    harmless = deployment.harmless_variables
    read = {
        name
        for name in assigned - (harmless - {"GH_REPO"})
        if not name.isidentifier() or name.startswith(READ_PREFIXES) or name in READ_VARIABLES
    }
    expanded = {name for word in words for name in REFERENCE.findall(word)} & assigned
    if settings := read | expanded | (environment.keys() - harmless):
        raise Unfollowable(f"{', '.join(sorted(settings))} may change which repository or login a call uses")


def bash_targets(command, cwd, deployment=Deployment.SINGLE_USER):
    """Every place on GitHub the command reaches. A subshell or an unquoted command
    substitution is followed inside; its directory and settings end where it closes."""
    # Any program may write a file git reads, by any path the text names.
    moves = [Moves(f"{found.group(0)} names a file git reads settings or hooks from") for found in GIT_FILE.finditer(command)][:1]
    command = without_heredocs(command)
    if any(names_git(inner) for inner in BACKTICKS.findall(command)):
        raise Unfollowable("a command substitution runs a git or gh call this provider cannot follow")
    targets, pushed, exported, assigned, directory, scopes = [], False, {}, set(), cwd, []
    for words in segments_of(command):
        match words:
            case "(":
                scopes.append((exported, set(assigned), directory))
                continue
            case ")":
                # A `)` that closes no subshell ends a `case` pattern.
                if scopes:
                    exported, assigned, directory = scopes.pop()
                continue
        environment = dict(exported)
        while words and words[0] in PREFIXES:
            words = words[1:]
        if scopes and "case" in words:
            raise Unfollowable("a case inside a subshell closes patterns this provider cannot tell from the subshell")
        if words[:1] == ["function"]:
            raise Unfollowable("function defines a function whose calls this provider cannot follow")
        exporting = words[:1] == ["export"]
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
        wrapper, words = words[0], unwrapped(words)
        if not words:
            continue
        program = os.path.basename(words[0])
        # A wrapper runs a builtin as a child, whose directory ends with it.
        if wrapper in WRAPPERS and program in ("cd", "pushd", "popd"):
            continue
        if program in ("git", "gh") and program != words[0]:
            raise Unfollowable(f"{words[0]} may not be the {program} this provider follows")
        if program in ("git", "gh"):
            unsafe_settings(words, environment, assigned, deployment)
        match program:
            case "git":
                pushed |= "push" in words
                match git_target(words[1:], directory):
                    case Moves() as moved:
                        moves.append(moved)
                    case Target() as target:
                        targets.append(target)
            case "gh":
                targets += gh_targets(words[1:], environment, directory)
            case "cd" | "pushd" if len(words) == 2:
                directory = within(directory, words[1])
            case "cd" | "pushd" | "popd":
                raise Unfollowable(f"{' '.join(words)} moves to a directory this provider cannot follow")
            case _ if program in INTERPRETERS:
                raise Unfollowable(f"{program} runs commands this provider cannot follow")
            case _ if program in SETTERS or program == "printf" and any(word.startswith("-v") for word in words):
                assigned.add(f"a variable {program} sets")
            case "[" | "[[" | "test":
                pass
            case _ if (
                GLOB & set(program)
                or "$" in program
                or "`" in program
                or any(os.path.basename(word) in ("git", "gh") or INVOCATION.search(word) for word in words)
            ):
                raise Unfollowable(f"{program} runs a git or gh call this provider cannot follow")
    # A loop can run a change after the push it precedes, so order does not matter.
    if moves and (pushed or any(target.kind != "slug" for target in targets)):
        raise Unfollowable(f"{moves[0].change} changes where a push or the checkout's repository goes")
    return list(dict.fromkeys(targets))


def count(arguments, key, default):
    """A positive integer argument, `None` for any other value."""
    value = arguments.get(key, default)
    return value if isinstance(value, int) and not isinstance(value, bool) and value > 0 else None


def mcp_issue_listing(arguments):
    """`list_issues`: the MCP server reads the same GraphQL connection, so
    its filters, order, and cursor repeat as they are."""
    unknown = Listing("issues", UNKNOWN)
    text = {key: arguments.get(key) or "" for key in ("state", "orderBy", "direction", "since")}
    labels, fields, after = arguments.get("labels") or [], arguments.get("field_filters"), arguments.get("after")
    first = count(arguments, "perPage", 30)
    if (
        not all(isinstance(value, str) for value in text.values())
        or not (isinstance(labels, list) and all(isinstance(label, str) for label in labels))
        or not (after is None or isinstance(after, str))
        or first is None
        or first > MAX_FIRST
        or "page" in arguments
    ):
        return unknown
    state, order, direction, since = text["state"].upper(), text["orderBy"].upper(), text["direction"].upper(), text["since"]
    if DATE.match(since):
        since += "T00:00:00Z"
    if since and not DATE_TIME.match(since):
        return unknown
    return listing(
        "issues",
        # Custom issue-field filters only narrow the listing; they are left out.
        not fields,
        first,
        states=(state,) if state in ("OPEN", "CLOSED") else ("OPEN", "CLOSED"),
        labels=tuple(labels),
        order=(order if order in ISSUE_ORDERS else "CREATED_AT", direction if direction in ("ASC", "DESC") else "DESC"),
        filters=(("since", since),) if since else (),
        after=after or None,
    )


def mcp_pull_request_listing(arguments):
    """`list_pull_requests`: the MCP server lists through REST, page by
    page; the query reads every page up to the one asked for."""
    unknown = Listing("pull_requests", UNKNOWN)
    text = {key: arguments.get(key) or "" for key in ("state", "head", "base", "sort", "direction")}
    per_page, page = count(arguments, "perPage", 30), count(arguments, "page", 1)
    if not all(isinstance(value, str) for value in text.values()) or per_page is None or page is None:
        return unknown
    states = REST_PULL_REQUEST_STATES.get(text["state"] or "open")
    sort = text["sort"] or "created"
    direction = text["direction"] or ("desc" if sort == "created" else "asc")
    if states is None or sort not in REST_SORTS or direction not in ("asc", "desc"):
        return unknown
    # REST's `head` is `owner:branch`; GraphQL filters on the branch alone, a wider set.
    head = text["head"].rpartition(":")[2] if ":" in text["head"] else None
    return listing(
        "pull_requests",
        REST_SORTS[sort] is not None and not text["head"],
        min(per_page, MAX_FIRST) * page,
        states=states,
        order=(REST_SORTS[sort] or "CREATED_AT", direction.upper()),
        base=text["base"] or None,
        head=head or None,
    )


MCP_LISTINGS = {"list_issues": mcp_issue_listing, "list_pull_requests": mcp_pull_request_listing}


def mcp_target(tool, arguments):
    """The repository, and the pull request, issue, or listing a GitHub MCP
    call names."""
    owner, repo = arguments.get("owner"), arguments.get("repo")
    if not all(isinstance(value, str) and value and "/" not in value and not value.startswith("$") for value in (owner, repo)):
        return None
    numbers = [arguments[key] for key in NUMBER_ARGUMENTS if key in arguments]
    number = next((value for value in numbers if isinstance(value, int) and not isinstance(value, bool) and value > 0), None)
    listed = MCP_LISTINGS.get(tool.removeprefix("mcp/github/"))
    return Target("slug", f"{owner}/{repo}", None, number, listed(arguments) if listed else None)


def call_targets(artifact, deployment=Deployment.SINGLE_USER):
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
    if not isinstance(command, str) or not names_git(command):
        return []
    cwd = artifact.get("cwd")
    return bash_targets(command, cwd if isinstance(cwd, str) else None, deployment)


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


def within_budget(limit):
    """`limit` seconds, or what is left of the consult's budget when the runtime set one
    and less is left."""
    budget = os.environ.get(CONSULT_TIMEOUT, "")
    if not budget.isdigit():
        return limit
    left = STARTED + int(budget) / 1000 - ANSWER_MARGIN_SECONDS - time.monotonic()
    if left <= 0:
        raise RuntimeError(f"the consult's {budget} ms budget ran out")
    return min(limit, left)


def git(directory, *arguments):
    completed = subprocess.run(
        ["git", "-C", directory, *arguments],
        capture_output=True,
        text=True,
        timeout=within_budget(GIT_TIMEOUT_SECONDS),
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
    """The one repository, at most one pull request or issue, and at most
    one listing the call reaches, or `None` when it reaches nothing on
    GitHub."""
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
    listings = {target.listing for target in targets if target.listing}
    if len(listings) > 1:
        raise Unfollowable(f"the command reads {len(listings)} listings")
    slug = min(slug for slug, _ in places)
    return slug, next(iter(numbers), None), next(iter(listings), None)


# ---------------------------------------------------------------- GitHub

ACCOUNT = "__typename ... on User { databaseId } ... on Bot { databaseId }"


def query_of(deployment):
    match deployment:
        case Deployment.SINGLE_USER:
            author, editor, user = "login __typename", "login", "login"
            viewer, viewer_permission = "viewer { login }\n  ", " viewerPermission"
        case Deployment.MULTI_USER:
            author, editor, user = ACCOUNT, ACCOUNT, "databaseId"
            viewer, viewer_permission = "", ""
    actor = f"author {{ {author} }} authorAssociation editor {{ {editor} }} lastEditedAt"
    listed = f"pageInfo {{ hasNextPage }} nodes {{ number {actor} }}"
    return f"""
query(
  $owner: String!, $name: String!, $number: Int!, $withNumber: Boolean!,
  $withIssues: Boolean!, $withPullRequests: Boolean!, $first: Int!, $after: String,
  $issueStates: [IssueState!], $pullRequestStates: [PullRequestState!], $labels: [String!],
  $orderBy: IssueOrder, $filterBy: IssueFilters, $baseRefName: String, $headRefName: String
) {{
  {viewer}repository(owner: $owner, name: $name) {{
    nameWithOwner visibility{viewer_permission} parent {{ nameWithOwner }}
    issues(
      first: $first, after: $after, states: $issueStates, labels: $labels, orderBy: $orderBy, filterBy: $filterBy
    ) @include(if: $withIssues) {{ {listed} }}
    pullRequests(
      first: $first, after: $after, states: $pullRequestStates, labels: $labels, orderBy: $orderBy,
      baseRefName: $baseRefName, headRefName: $headRefName
    ) @include(if: $withPullRequests) {{ {listed} }}
    issueOrPullRequest(number: $number) @include(if: $withNumber) {{
      __typename
      ... on Issue {{
        number locked {actor}
        comments(first: 100) {{ pageInfo {{ hasNextPage }} nodes {{ {actor} }} }}
      }}
      ... on PullRequest {{
        number locked isCrossRepository {actor}
        comments(first: 100) {{ pageInfo {{ hasNextPage }} nodes {{ {actor} }} }}
        reviews(first: 50) {{ pageInfo {{ hasNextPage }} nodes {{
          {actor}
          comments(first: 30) {{ pageInfo {{ hasNextPage }} nodes {{ {actor} }} }}
        }} }}
        commits(first: 100) {{ pageInfo {{ hasNextPage }} nodes {{ commit {{
          authors(first: 10) {{ pageInfo {{ hasNextPage }} nodes {{ user {{ {user} }} }} }}
        }} }} }}
      }}
    }}
  }}
}}
"""


def variables_of(slug, number, listed=None):
    owner, name = slug.split("/")
    fetched = listed if listed is not None and listed.scope != UNKNOWN else Listing("", UNKNOWN)
    return {
        "owner": owner,
        "name": name,
        "number": number or 0,
        "withNumber": number is not None,
        "withIssues": fetched.kind == "issues",
        "withPullRequests": fetched.kind == "pull_requests",
        "first": fetched.first,
        "after": fetched.after,
        "issueStates": list(fetched.states) if fetched.kind == "issues" else None,
        "pullRequestStates": list(fetched.states) if fetched.kind == "pull_requests" else None,
        "labels": list(fetched.labels) or None,
        "orderBy": {"field": fetched.order[0], "direction": fetched.order[1]},
        "filterBy": dict(fetched.filters) or None,
        "baseRefName": fetched.base,
        "headRefName": fetched.head,
    }


def graphql_url(root):
    """GitHub's GraphQL endpoint beside a REST root: `/api/graphql` for an
    Enterprise Server's `/api/v3`, `/graphql` otherwise."""
    return root.removesuffix("/v3") + "/graphql" if root.endswith("/api/v3") else f"{root}/graphql"


def graphql(token, query, variables):
    request = urllib.request.Request(
        graphql_url(api_root(os.environ)),
        data=json.dumps({"query": query, "variables": variables}).encode(),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method="POST",
    )
    limit = within_budget(TIMEOUT_SECONDS)
    outcome = {}

    def exchange():
        try:
            with urllib.request.urlopen(request, timeout=limit) as response:
                outcome["body"] = response.read()
        except Exception as error:  # noqa: BLE001 - re-raised by the caller
            outcome["error"] = error

    # A socket timeout bounds each read only; the join bounds the whole exchange, and a
    # daemon thread still blocked in it ends with the helper.
    worker = threading.Thread(target=exchange, daemon=True)
    worker.start()
    worker.join(limit)
    match outcome:
        case {"body": body}:
            return json.loads(body)
        case {"error": urllib.error.HTTPError() as error}:
            raise RuntimeError(f"GitHub GraphQL failed: {error.code}") from error
        case {"error": error}:
            raise error
        case _:
            raise RuntimeError(f"GitHub GraphQL did not answer within {limit:.1f} s")


def connection(node, name):
    """A connection's nodes and whether it had more pages than were read."""
    found = node.get(name) or {}
    return found.get("nodes") or [], bool((found.get("pageInfo") or {}).get("hasNextPage"))


def item_of(node, deployment):
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

    key, person = deployment.key, deployment.person
    participants, seen, editors = [], set(), []
    for text in written:
        author = text.get("author") or {}
        who = person(author)
        if who not in seen:
            seen.add(who)
            participants.append(
                {key: who, "association": text.get("authorAssociation"), "bot": author.get("__typename") == "Bot"}
            )
        if text.get("lastEditedAt"):
            editor = person(text.get("editor") or {})
            if editor not in editors:
                editors.append(editor)

    author = node.get("author") or {}
    item = {
        "number": node.get("number"),
        "author": {key: person(author), "association": node.get("authorAssociation")},
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
                who = person(git_author.get("user") or {})
                if who not in commit_authors:
                    commit_authors.append(who)
        item["commit_authors"] = commit_authors
    item["last_editors"] = editors
    item["truncated"] = truncated
    return item


def listed_item(node, deployment):
    """The facts about one listed pull request or issue: its author, and its
    last editor when it was edited (`null` for a deleted account)."""
    author = node.get("author") or {}
    item = {
        "number": node.get("number"),
        "author": {deployment.key: deployment.person(author), "association": node.get("authorAssociation")},
        "bot": author.get("__typename") == "Bot",
    }
    if node.get("lastEditedAt"):
        item["last_editor"] = deployment.person(node.get("editor") or {})
    return item


CONNECTIONS = {"issues": "issues", "pull_requests": "pullRequests"}


def listing_of(repository, listed, deployment):
    """The listed items, and whether they may miss one the call returns."""
    if listed.scope == UNKNOWN:
        return {"items": [], "truncated": True}
    if not isinstance(repository.get(CONNECTIONS[listed.kind]), dict):
        raise RuntimeError(f"GitHub GraphQL: no {listed.kind} in the response")
    nodes, more = connection(repository, CONNECTIONS[listed.kind])
    return {"items": [listed_item(node, deployment) for node in nodes], "truncated": listed.scope == WHOLE and more}


def answer_of(payload, listed=None, deployment=Deployment.SINGLE_USER):
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
    facts = {"name": repository.get("nameWithOwner"), "visibility": visibility}
    if deployment.has_viewer:
        facts["viewer_permission"] = repository.get("viewerPermission")
    facts["fork_of"] = (repository.get("parent") or {}).get("nameWithOwner")
    answer = {"viewer": (data.get("viewer") or {}).get("login")} if deployment.has_viewer else {}
    answer["repository"] = facts
    node = repository.get("issueOrPullRequest")
    match (node or {}).get("__typename"):
        case "PullRequest":
            answer["pull_request"] = item_of(node, deployment)
        case "Issue":
            answer["issue"] = item_of(node, deployment)
    if listed is not None:
        answer[listed.kind] = listing_of(repository, listed, deployment)
    return answer


def context_of(consult, deployment=Deployment.SINGLE_USER):
    """The answer for one consult: `None` for a call that reaches nothing on
    GitHub, else the facts one GraphQL query returns."""
    if not isinstance(consult, dict) or consult.get("version") != 1 or consult.get("kind") != "context":
        raise ValueError("the consult must be a version 1 context consult")
    artifact = consult.get("artifact")
    if not isinstance(artifact, dict):
        raise ValueError("the consult carries no artifact")
    place = reached(call_targets(artifact, deployment))
    if place is None:
        return None
    payload = graphql(deployment.token(), query_of(deployment), variables_of(*place))
    return answer_of(payload, place[2], deployment)


def main(deployment):
    try:
        raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
        if len(raw) > MAX_INPUT_BYTES:
            raise ValueError("the consult is too large")
        json.dump({"version": 1, "answer": context_of(json.loads(raw), deployment)}, sys.stdout)
        sys.stdout.write("\n")
    except Exception as error:
        print(f"github context: {error}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main(Deployment.SINGLE_USER)
