"""The mock corporate systems, as plain folders on disk.

A direct Python port of the sibling Rust demo's ``systems.rs`` so the two
demos run over the *same* corpus and the *same* planted injection — the only
difference between them is the defense (OpenAPPA's policy engine there, FIDES
here). Each :class:`System` is a subdirectory holding ``.md``/``.txt`` files;
the three verbs — :func:`search`, :func:`read`, :func:`create` — plus the
:func:`send_email` sink are the whole behaviour. The FIDES tool wrappers in
``tools.py`` stay thin delegators, exactly like ``server.rs``.

All file names that reach the filesystem come from the model (untrusted), so
every entry point runs them through :func:`validate_file_name` first.

The corpus is read-only and defaults to the sibling ``corporate-agent/data``
tree; the ``send_email`` sink writes into this demo's own ``data/email/`` so
the two demos never fight over one observable folder.
"""

from __future__ import annotations

import os
import re
import time
from dataclasses import dataclass
from enum import Enum
from pathlib import Path

_PACKAGE_DIR = Path(__file__).resolve().parent
_CRATE_DIR = _PACKAGE_DIR.parent
# The sibling Rust demo owns the canonical corpus + planted injection thread.
_SIBLING_CORPUS = (_CRATE_DIR / ".." / "corporate-agent" / "data").resolve()


class System(str, Enum):
    """One mock internal system, backed by a subdirectory of the data root."""

    HR = "hr"
    FINANCE = "finance"
    TASK_TRACKER = "task_tracker"
    PUBLIC_FORUM = "public_forum"
    EMAIL = "email"

    @property
    def dir_name(self) -> str:
        return self.value


@dataclass(frozen=True)
class Hit:
    """A single search hit: the file it matched and the first matching line."""

    file: str
    snippet: str


class NameError_(ValueError):
    """A file name supplied by the model was unsafe."""


def resolve_corpus_root(explicit: str | os.PathLike[str] | None = None) -> Path:
    """Where the ``search``/``read`` verbs look. Explicit override, else
    ``CORP_DATA_ROOT``, else the sibling ``corporate-agent/data`` corpus."""
    if explicit is not None:
        return Path(explicit).resolve()
    env = os.environ.get("CORP_DATA_ROOT", "").strip()
    if env:
        return Path(env).resolve()
    return _SIBLING_CORPUS


def resolve_sink_root(explicit: str | os.PathLike[str] | None = None) -> Path:
    """Where ``send_email`` drops its files — this demo's own writable
    ``data/`` dir by default, so the shared read-only corpus stays pristine and
    the leak is observed *here*."""
    if explicit is not None:
        return Path(explicit).resolve()
    env = os.environ.get("CORP_SINK_ROOT", "").strip()
    if env:
        return Path(env).resolve()
    return _CRATE_DIR / "data"


def validate_file_name(name: str) -> None:
    """Reject anything that could escape the system's directory or hide as a
    dotfile. Model-supplied input — validated at this single choke point."""
    stripped = name.strip()
    if not stripped:
        raise NameError_(f"invalid file name {name!r}: empty")
    if "/" in name or "\\" in name:
        raise NameError_(f"invalid file name {name!r}: contains a path separator")
    if ".." in name:
        raise NameError_(f"invalid file name {name!r}: contains '..'")
    if name.startswith("."):
        raise NameError_(f"invalid file name {name!r}: starts with '.'")
    if Path(name).is_absolute():
        raise NameError_(f"invalid file name {name!r}: is an absolute path")


def _list_files(directory: Path) -> list[tuple[str, str]]:
    """Every ``.md``/``.txt`` file in ``directory``, sorted by name, as
    ``(name, body)``. A folder that does not exist yet reads as empty."""
    if not directory.is_dir():
        return []
    out: list[tuple[str, str]] = []
    for entry in sorted(directory.iterdir(), key=lambda p: p.name):
        if entry.is_file() and entry.suffix in (".md", ".txt"):
            out.append((entry.name, entry.read_text(encoding="utf-8", errors="replace")))
    return out


def _available_names(directory: Path) -> str:
    files = _list_files(directory)
    return ", ".join(n for n, _ in files) if files else "(none)"


def _truncate(s: str, limit: int = 200) -> str:
    return s if len(s) <= limit else s[:limit] + "…"


def _first_line(body: str) -> str:
    for line in body.splitlines():
        if line.strip():
            return _truncate(line.strip())
    return ""


def search(root: Path, system: System, query: str) -> list[Hit]:
    """Case-insensitive substring search over file names and contents. One hit
    per matching file; its snippet is the first matching line (trimmed)."""
    needle = query.strip().lower()
    directory = root / system.dir_name
    hits: list[Hit] = []
    for name, body in _list_files(directory):
        if not needle:
            hits.append(Hit(file=name, snippet=_first_line(body)))
            continue
        if needle in name.lower():
            hits.append(Hit(file=name, snippet=_first_line(body)))
            continue
        for line in body.splitlines():
            if needle in line.lower():
                hits.append(Hit(file=name, snippet=_truncate(line.strip())))
                break
    return hits


def read(root: Path, system: System, file: str) -> str:
    """Full contents of a named file. Not-found lists the available files so
    the model can correct itself."""
    validate_file_name(file)
    directory = root / system.dir_name
    path = directory / file
    if not path.is_file():
        raise FileNotFoundError(
            f"no file named {file!r} in the {system.dir_name} system; "
            f"available: {_available_names(directory)}"
        )
    return path.read_text(encoding="utf-8", errors="replace")


def create(root: Path, system: System, file: str, content: str) -> None:
    """Write a new file. Refuses to overwrite an existing one."""
    validate_file_name(file)
    directory = root / system.dir_name
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / file
    if path.exists():
        raise FileExistsError(f"a file named {file!r} already exists in the {system.dir_name} system")
    path.write_text(content, encoding="utf-8")


def _slug(subject: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", subject.lower()).strip("-")
    s = s[:40]
    return s or "message"


def send_email(sink_root: Path, to: str, subject: str, body: str) -> str:
    """The mocked outbound email sink: writes the message as a file into the
    ``email/`` folder and returns the saved file name. There is no
    ``read``/``search`` counterpart — the folder is purely the observable
    side-effect the injection demo inspects."""
    directory = sink_root / System.EMAIL.dir_name
    directory.mkdir(parents=True, exist_ok=True)
    stamp = int(time.time())
    file = f"{stamp}-{_slug(subject)}.md"
    (directory / file).write_text(f"To: {to}\nSubject: {subject}\n\n{body}\n", encoding="utf-8")
    return file
