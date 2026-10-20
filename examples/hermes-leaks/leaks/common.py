"""State and sinks shared by the scenario tools.

The runner sets APPA_DEMO_STATE, the directory holding state.json (the
scenario fixture) and sinks.json (everything a tool sent somewhere).
"""

import json
import os
import subprocess
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

Handler = Any
Tool = tuple[dict[str, Any], Handler]


@dataclass(frozen=True)
class Scenario:
    name: str
    prompt: str
    fixture: dict[str, Any]
    tools: list[Tool]
    markers: tuple[str, ...]
    leak_sinks: tuple[str, ...]
    """Sink tools whose recipients must not see a marker."""
    done: tuple[str, ...]
    """Sink tools whose use means the agent finished the task."""
    toolsets: tuple[str, ...] = ()
    """Hermes built-in toolsets the agent gets besides the scenario's own tools."""
    workspace: Callable[[Path], None] | None = None
    """Builds a real working directory the agent runs in."""
    collect: Callable[[Path], list[dict[str, str]]] | None = None
    """Reads what reached the scenario's sinks from the working directory after the run."""


def _state_dir() -> Path:
    return Path(os.environ["APPA_DEMO_STATE"])


def load() -> dict[str, Any]:
    return json.loads((_state_dir() / "state.json").read_text())


def save(state: dict[str, Any]) -> None:
    (_state_dir() / "state.json").write_text(json.dumps(state, indent=2))


def sink(tool: str, recipient: str, content: str) -> None:
    path = _state_dir() / "sinks.json"
    sent = json.loads(path.read_text()) if path.exists() else []
    sent.append({"tool": tool, "recipient": recipient, "content": content})
    path.write_text(json.dumps(sent, indent=2))


def schema(name: str, description: str, properties: dict[str, Any], required: list[str]) -> dict[str, Any]:
    return {
        "name": name,
        "description": description,
        "parameters": {"type": "object", "properties": properties, "required": required},
    }


STRING = {"type": "string"}


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, check=True).stdout


def _remote(workdir: Path, url: str) -> Path:
    """Where pushes to `url` land: a bare repository whose path reads like the GitHub URL."""
    return workdir.parent / (url.removeprefix("https://") + ".git")


def public_repo(workdir: Path, files: dict[str, str], url: str, executable: tuple[str, ...] = ()) -> None:
    """A git checkout whose origin looks like `url`; pushes land in a bare repo beside it."""
    remote = _remote(workdir, url)
    remote.parent.mkdir(parents=True)
    git(workdir.parent, "init", "-q", "--bare", "-b", "main", str(remote))
    workdir.mkdir()
    for path, text in files.items():
        (workdir / path).parent.mkdir(parents=True, exist_ok=True)
        (workdir / path).write_text(text)
    for path in executable:
        (workdir / path).chmod(0o755)
    git(workdir, "init", "-q", "-b", "main")
    git(workdir, "config", "user.name", "Jordan Dev")
    git(workdir, "config", "user.email", "jordan@example.com")
    git(workdir, "config", f"url.{remote}.insteadOf", url)
    git(workdir, "remote", "add", "origin", url)
    git(workdir, "add", "-A")
    git(workdir, "commit", "-q", "-m", "Initial import")
    git(workdir, "push", "-q", "origin", "main")


def pushed(workdir: Path) -> list[dict[str, str]]:
    """Everything pushed after the initial import, as one `git_push` sink."""
    url = git(workdir, "config", "--get", "remote.origin.url").strip()
    remote = _remote(workdir, url)
    log = git(remote, "log", "--all", "-p", "--format=\x1e%H %s", "--binary")
    commits = log.split("\x1e")
    later = [c for c in commits if c.strip() and "Initial import" not in c.splitlines()[0]]
    return [{"tool": "git_push", "recipient": f"{url} (public)", "content": c} for c in later]
