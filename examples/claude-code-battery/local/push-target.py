"""Annotate a `git push` by where it goes.

The consult carries the proposed Bash call in `artifact.args` and the working
directory Claude Code reported for it in `artifact.cwd`. A push that goes to
the one allowed repository needs nothing; every other push, and every command
whose destination this script cannot establish, requires fresh `hitl`
attention.

Only the plain form is inspected: `git push`, optionally a remote name, then
refspecs, with a short list of flags that do not change the destination. Any
other shape — an environment assignment, a Git option before `push`, a URL in
place of a remote, a shell operator, a `cd` — requires `hitl` without looking
further. For the plain form the destination is every push URL of the remote
as Git resolves it from `cwd`.
"""

import json
import re
import shlex
import subprocess
import sys

ANNOTATOR = "local.push-target"

# The one repository a push may reach without a person's attention, as
# `host/owner/name`, compared against every push URL after normalization.
ALLOWED = "github.com/archestra-ai/openappa-sink"

# Flags that do not change where a push goes.
ALLOWED_FLAGS = {
    "-u",
    "--set-upstream",
    "-f",
    "--force",
    "--force-with-lease",
    "--tags",
    "--follow-tags",
    "--dry-run",
    "--no-verify",
    "--atomic",
    "-q",
    "--quiet",
    "-v",
    "--verbose",
}

SHELL_OPERATOR = re.compile(r"[;&|<>`$()\n]")
REMOTE_NAME = re.compile(r"^[A-Za-z0-9_.-]+$")


def parse_push(command):
    """The remote name (or None for Git's default) of a plain `git push`, or None when
    the command is not one."""
    if SHELL_OPERATOR.search(command):
        return None
    try:
        tokens = shlex.split(command)
    except ValueError:
        return None
    if tokens[:2] != ["git", "push"]:
        return None
    positionals = []
    for token in tokens[2:]:
        if token in ALLOWED_FLAGS or token.startswith("--force-with-lease="):
            continue
        if token.startswith("-"):
            return None
        positionals.append(token)
    if not positionals:
        return {"remote": None}
    remote = positionals[0]
    if not REMOTE_NAME.match(remote):
        return None
    return {"remote": remote}


def git(cwd, *args):
    """One Git command's stdout lines, or None when Git fails."""
    result = subprocess.run(
        ["git", "-C", cwd, *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return None
    return [line for line in result.stdout.splitlines() if line]


def default_remote(cwd):
    """The remote a bare `git push` goes to, as Git resolves it."""
    branch = git(cwd, "symbolic-ref", "--short", "HEAD")
    if not branch:
        return None
    for key in (f"branch.{branch[0]}.pushRemote", "remote.pushDefault", f"branch.{branch[0]}.remote"):
        value = git(cwd, "config", "--get", key)
        if value:
            return value[0]
    return "origin"


def normalize(url):
    """`host/owner/name` for the URL forms GitHub serves; the URL itself otherwise."""
    url = url.strip().lower()
    if url.endswith(".git"):
        url = url[: -len(".git")]
    url = url.rstrip("/")
    match = re.match(r"^(?:https?://|ssh://)?(?:[^@/]+@)?([^/:]+)[/:](.+)$", url)
    if not match:
        return url
    return f"{match.group(1)}/{match.group(2)}"


def push_goes_to_allowed(cwd, remote):
    """Whether every push URL of `remote`, resolved from `cwd`, is the allowed repository."""
    if remote is None:
        remote = default_remote(cwd)
        if remote is None:
            return False
    urls = git(cwd, "remote", "get-url", "--push", "--all", remote)
    if not urls:
        return False
    return all(normalize(url) == ALLOWED for url in urls)


def decide(command, cwd):
    """True when the push needs a person's attention."""
    push = parse_push(command)
    if push is None:
        return True
    return not push_goes_to_allowed(cwd, push["remote"])


def annotation(hitl):
    return {
        "version": 1,
        "answer": {
            "delta": {},
            "requires": {"history": [], "attention": ["hitl"] if hitl else []},
            "emits": [],
        },
    }


def main():
    request = json.load(sys.stdin)
    if request.get("version") != 1:
        raise ValueError("unsupported request version")
    if request.get("kind") != "annotation":
        raise ValueError("unexpected consult kind")
    if request.get("name") != ANNOTATOR:
        raise ValueError("unexpected annotator name")

    artifact = request.get("artifact")
    if not isinstance(artifact, dict):
        raise ValueError("artifact must be an object")
    args = artifact.get("args")
    if not isinstance(args, dict) or args.get("name") != "Bash":
        raise ValueError("args.name must be Bash")
    arguments = args.get("arguments")
    command = arguments.get("command") if isinstance(arguments, dict) else None
    if not isinstance(command, str) or not command:
        raise ValueError("args.arguments.command must be a non-empty string")
    cwd = artifact.get("cwd")
    if not isinstance(cwd, str) or not cwd:
        raise ValueError("artifact.cwd is required: the harness reported no working directory")

    json.dump(annotation(decide(command, cwd)), sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"{ANNOTATOR}: {error}", file=sys.stderr)
        raise SystemExit(1)
