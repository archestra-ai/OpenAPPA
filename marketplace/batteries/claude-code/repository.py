"""The `claude-code.repository` input: which GitHub repository a command reaches.

One consult in (`kind = "input"`, the proposed Bash call and the directory
the harness would run it in), one answer out:

  {"name_with_owner": "acme/api", "visibility": "public"}
  {"name_with_owner": null, "visibility": null, "reason": "..."}

The repository is the one the command names — `--repo`/`-R` on a `gh`
call, a URL or a remote on `git push` — else the checkout's own `origin`,
asked of the GitHub CLI's login (`gh repo view`) in the harness's
directory. Whatever cannot be established answers `null` with a reason
and a zero exit: the annotator then reads the finding, not a guess.
Only a transport failure — a crash, a timeout — is a refused answer.
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


def unknown(reason):
    return {"name_with_owner": None, "visibility": None, "reason": reason}


def words_of(command):
    try:
        return shlex.split(command, comments=False, posix=True)
    except ValueError:
        return command.split()


def repository_named(command):
    """The repository the command itself names, or `None` when it names none.

    `gh ... --repo OWNER/NAME` (or `-R`) names it outright; `git push <url>`
    names it by URL; `git push <remote>` names a remote the checkout resolves.
    """
    words = words_of(command)
    for index, word in enumerate(words):
        if word in ("--repo", "-R") and index + 1 < len(words):
            return {"slug": words[index + 1]}
        if word.startswith("--repo="):
            return {"slug": word.partition("=")[2]}
    match = re.search(r"(?:^|[;&|(]\s*)git\s+(?:-C\s+\S+\s+)?push\b(.*?)(?:$|[;&|)])", command)
    if match:
        for word in words_of(match.group(1)):
            if word.startswith("-"):
                continue
            if "://" in word or word.startswith("git@"):
                return {"slug": word}
            return {"remote": word}
    return None


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


def repository_of(consult):
    """The finding for one consult."""
    artifact = consult.get("artifact") if isinstance(consult, dict) else None
    if not isinstance(artifact, dict) or consult.get("kind") != "input":
        raise ValueError("the consult must be an input consult with an artifact")
    arguments = artifact.get("arguments")
    command = arguments.get("command") if isinstance(arguments, dict) else None
    if not isinstance(command, str):
        return unknown("the call runs no command")
    cwd = artifact.get("cwd")
    if isinstance(cwd, str) and not os.path.isdir(cwd):
        cwd = None
    named = repository_named(command)
    if named is None and cwd is None:
        return unknown("the command names no repository and the harness reported no directory")
    try:
        target = []
        if named and "slug" in named:
            target = [named["slug"]]
        elif named and "remote" in named:
            if cwd is None:
                return unknown(f"the command pushes to remote {named['remote']} and the harness reported no directory")
            target = [remote_url(named["remote"], cwd)]
        found = json.loads(gh(["repo", "view", *target, "--json", "nameWithOwner,visibility"], cwd))
    except FileNotFoundError:
        return unknown("the GitHub CLI (gh) is not installed")
    except (subprocess.TimeoutExpired, RuntimeError, json.JSONDecodeError) as error:
        return unknown(str(error) or type(error).__name__)
    visibility = VISIBILITY.get(str(found.get("visibility", "")).upper())
    if visibility is None:
        return unknown("gh reported no visibility")
    return {"name_with_owner": found.get("nameWithOwner"), "visibility": visibility}


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
