"""The `claude-code.repository` input: which GitHub repository a command reaches.

One consult in (`kind = "input"`, the proposed Bash call and the directory
the harness would run it in), one answer out:

  {"name_with_owner": "acme/api", "visibility": "public"}
  {"name_with_owner": null, "visibility": null, "reason": "..."}

The command is split into its simple commands, and every `git push` and
`gh` call among them names a destination: `--repo`/`-R`, `GH_REPO`, a
`repos/OWNER/NAME` API path, a GitHub URL, or an `OWNER/NAME` word of
`gh repo` on a `gh` call, a URL or a
remote on a push (in the `-C` or `cd` directory when given), else the checkout's
own repository. Each is asked of the GitHub CLI's login (`gh repo view`);
several destinations answer the most widely readable one. A call this
input cannot follow — a shell, `eval`, `source` or subshell, `git -c`,
an environment setting such as `GIT_DIR`, a computed program or target —
and whatever else cannot be established answers `null` with a
reason and a zero exit: the annotator then reads the finding, not a
guess. Only a transport failure — a crash, a timeout — is a refused
answer.
"""

import json
import os
import re
import shlex
import subprocess
import sys

MAX_INPUT_BYTES = 64 * 1024
GH_TIMEOUT_SECONDS = 4
VISIBILITY = {"PUBLIC": "public", "PRIVATE": "private", "INTERNAL": "internal"}
MOST_READABLE_FIRST = ["public", "internal", "private"]

OPERATORS = "();<>&|\n"
PREFIXES = {"sudo", "env", "command", "exec", "nohup", "time", "then", "do", "else", "!", "{"}
INTERPRETERS = {"bash", "sh", "zsh", "dash", "ksh", "mksh", "csh", "tcsh", "fish", "pwsh", "busybox", "eval", "source", ".", "xargs"}
GLOB = set("*?[")
SETTERS = {"declare", "typeset", "local", "readonly", "read", "mapfile", "readarray", "getopts", "alias"}
PUSH_OPTIONS_WITH_VALUE = {"-o", "--push-option", "--receive-pack", "--exec", "--repo"}
HARMLESS_GIT_OPTIONS = {"--no-pager", "--paginate", "-P", "--no-replace-objects"}
GIT_OPTIONS_WITH_VALUE = {"-c", "--git-dir", "--work-tree", "--namespace", "--config-env", "--exec-path"}
GITHUB_URL = re.compile(r"^(?:https?://|git@)github\.com[/:]([\w.-]+)/([\w.-]+?)(?:\.git)?(?:[/#?]|$)")
API_PATH = re.compile(r"^(?:https://api\.github\.com)?/?repos/([\w.-]+)/([\w.-]+)")
SLUG = re.compile(r"^[\w.-]+/[\w.-]+$")
SUBSTITUTION = re.compile(r"\$\(([^)]*)|`([^`]*)", re.DOTALL)
INVOCATION = re.compile(r"\b(git|gh)\s")
HARMLESS_VARIABLES = {"GH_REPO", "GH_PROMPT_DISABLED", "GH_PAGER", "GIT_PAGER", "PAGER", "GIT_TERMINAL_PROMPT", "NO_COLOR", "CI"}
MAX_TARGETS = 4


class Unfollowable(Exception):
    """The command reaches a repository this input cannot establish."""


def unknown(reason):
    return {"name_with_owner": None, "visibility": None, "reason": reason}


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


def named(word):
    if "$" in word or "`" in word:
        raise Unfollowable(f"{word} is computed when the command runs")
    match = GITHUB_URL.match(word)
    return f"{match.group(1)}/{match.group(2)}" if match else word


def git_target(words, directory):
    """What one `git` call pushes to, or `None` for a call that pushes nothing."""
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
    if words[index : index + 1] != ["push"]:
        return None
    if unfollowable:
        raise Unfollowable(f"git {unfollowable} changes which repository a push reaches")
    arguments = iter(words[index + 1 :])
    for word in arguments:
        if word in PUSH_OPTIONS_WITH_VALUE:
            next(arguments, None)
        elif not word.startswith("-"):
            if "://" in word or word.startswith("git@"):
                return ("slug", named(word), directory)
            return ("remote", named(word), directory)
    return ("default", None, directory)


def gh_targets(words, environment, directory):
    """What one `gh` call reaches. `--repo`/`-R` and `GH_REPO` choose the
    repository outright; otherwise the checkout's own counts too, beside
    any API path or URL among the words, since a word may be a title or a
    body rather than the destination."""
    chosen = [environment["GH_REPO"]] if "GH_REPO" in environment else []
    mentioned = []
    for index, word in enumerate(words):
        if word.startswith("--hostname"):
            raise Unfollowable("gh --hostname reaches a host other than github.com")
        if word in ("--repo", "-R") and index + 1 < len(words):
            chosen.append(words[index + 1])
        elif word.startswith(("--repo=", "-R")) and word not in ("--repo", "-R"):
            chosen.append(word.removeprefix("--repo=").removeprefix("-R").removeprefix("="))
        elif match := API_PATH.match(word):
            mentioned.append(f"{match.group(1)}/{match.group(2)}")
        elif GITHUB_URL.match(word) or words[0] == "repo" and SLUG.match(word):
            mentioned.append(word)
    targets = [("slug", named(slug), directory) for slug in chosen + mentioned]
    return targets if chosen else [*targets, ("default", None, directory)]


def within(directory, path):
    return os.path.join(directory, path) if directory else path


def repository_targets(command, cwd):
    """Every repository the command reaches, as `(kind, value, directory)`:
    a slug or URL, a remote of a checkout, or a checkout's own repository."""
    if any(INVOCATION.search("".join(inner)) for inner in SUBSTITUTION.findall(command)):
        raise Unfollowable("a command substitution runs a git or gh call this input cannot follow")
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
        if not words:
            assigned |= {name for name, value in environment.items() if exported.get(name) != value}
            continue
        program = os.path.basename(words[0])
        if program in ("git", "gh") and (settings := assigned | environment.keys() - HARMLESS_VARIABLES):
            raise Unfollowable(f"{', '.join(sorted(settings))} may change which repository or login a call uses")
        match program:
            case "git":
                target = git_target(words[1:], directory)
                targets += [target] if target else []
            case "gh":
                targets += gh_targets(words[1:], environment, directory)
            case "cd" | "pushd" if len(words) == 2 and words[1] != "-" and not {"$", "`", "~"} & set(words[1]):
                directory = within(directory, words[1])
            case "cd" | "pushd" | "popd":
                raise Unfollowable(f"{' '.join(words)} moves to a directory this input cannot follow")
            case _ if program in INTERPRETERS:
                raise Unfollowable(f"{program} runs commands this input cannot follow")
            case _ if program in SETTERS or program == "printf" and "-v" in words:
                raise Unfollowable(f"{program} sets a variable this input cannot follow")
            case _ if (
                GLOB & set(program)
                or "$" in program
                or "`" in program
                or any(os.path.basename(word) in ("git", "gh") or INVOCATION.search(word) for word in words)
            ):
                raise Unfollowable(f"{program} runs a git or gh call this input cannot follow")
    return list(dict.fromkeys(targets)) or [("default", None, directory)]


def gh(arguments, cwd):
    """`gh` run where the harness would run the command; the CLI's own login serves."""
    completed = subprocess.run(
        ["gh", *arguments],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=GH_TIMEOUT_SECONDS,
        check=False,
    )
    if completed.returncode:
        raise RuntimeError(completed.stderr.strip().splitlines()[-1] if completed.stderr.strip() else "gh failed")
    return completed.stdout


def remote_url(remote, cwd):
    completed = subprocess.run(
        ["git", "remote", "get-url", remote],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=GH_TIMEOUT_SECONDS,
        check=False,
    )
    if completed.returncode:
        raise RuntimeError(f"remote {remote} is not configured here")
    return completed.stdout.strip()


def finding_of(target):
    kind, value, directory = target
    if value and value.startswith("-"):
        return unknown(f"{value} is an option, not a repository")
    if kind != "slug" and not (directory and os.path.isabs(directory) and os.path.isdir(directory)):
        return unknown(f"the command reaches a checkout's {value or 'own'} repository and names no directory this input can read")
    try:
        match kind:
            case "slug":
                view = [value]
            case "remote":
                view = [remote_url(value, directory)]
            case "default":
                view = []
        found = json.loads(gh(["repo", "view", *view, "--json", "nameWithOwner,visibility"], directory))
    except FileNotFoundError:
        return unknown("the GitHub CLI (gh) is not installed")
    except (subprocess.TimeoutExpired, RuntimeError, json.JSONDecodeError) as error:
        return unknown(str(error) or type(error).__name__)
    visibility = VISIBILITY.get(str(found.get("visibility", "")).upper()) if isinstance(found, dict) else None
    if visibility is None:
        return unknown("gh reported no visibility")
    return {"name_with_owner": found.get("nameWithOwner"), "visibility": visibility}


def repository_of(consult):
    """The finding for one consult: the most widely readable repository the
    command reaches, or the first one that cannot be established."""
    artifact = consult.get("artifact") if isinstance(consult, dict) else None
    if not isinstance(artifact, dict) or consult.get("kind") != "input":
        raise ValueError("the consult must be an input consult with an artifact")
    arguments = artifact.get("arguments")
    command = arguments.get("command") if isinstance(arguments, dict) else None
    if not isinstance(command, str):
        return unknown("the call runs no command")
    cwd = artifact.get("cwd")
    if not (isinstance(cwd, str) and os.path.isdir(cwd)):
        cwd = None
    try:
        targets = repository_targets(command, cwd)
    except Unfollowable as error:
        return unknown(str(error))
    if len(targets) > MAX_TARGETS:
        return unknown(f"the command reaches more than {MAX_TARGETS} repositories")
    findings = []
    for target in targets:
        finding = finding_of(target)
        if finding["visibility"] is None:
            return finding
        findings.append(finding)
    return min(findings, key=lambda finding: MOST_READABLE_FIRST.index(finding["visibility"]))


def main():
    body = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(body) > MAX_INPUT_BYTES:
        sys.exit(2)
    consult = json.loads(body)
    if consult.get("version") != 1:
        sys.exit(2)
    json.dump({"version": 1, "answer": repository_of(consult)}, sys.stdout)


if __name__ == "__main__":
    main()
