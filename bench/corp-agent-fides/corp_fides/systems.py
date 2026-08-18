"""The connection to the shared mock corporate systems.

The systems themselves — ``hr``, ``finance``, ``task_tracker``,
``public_forum``, and ``vendor`` folders with ``search``/``read``/``create``
verbs plus ``send_email`` and ``share_legal_packet`` — live in the sibling Rust
``corp-systems`` crate as a stdio MCP server (``corp-systems-mcp``), which this
demo spawns. The sibling
APPA demo links that crate as a library and runs the same systems in-process;
both act over the *same* corpus and the *same* planted injection, and the only
variable between them is the defense (OpenAPPA's policy engine there, FIDES
here).

This module owns the plumbing: resolving the corpus/sink roots and the server
binary (building it on demand via cargo), and :class:`CorpSystemsClient`, a
thin async MCP client the FIDES-labeled tools in ``tools.py`` forward through.

The corpus is read-only; ``send_email`` writes into this demo's own
``data/email/`` (``--sink-root``) so the two demos never fight over one
observable folder.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from enum import Enum
from pathlib import Path
from typing import Any

from mcp import ClientSession, StdioServerParameters, types
from mcp.client.stdio import get_default_environment, stdio_client

_PACKAGE_DIR = Path(__file__).resolve().parent
_CRATE_DIR = _PACKAGE_DIR.parent
# The sibling crate owns the server, the canonical corpus, and the injection.
_CORP_SYSTEMS_DIR = (_CRATE_DIR / ".." / "corp-systems").resolve()


class System(str, Enum):
    """One mock internal system, backed by a subdirectory of the corpus root."""

    HR = "hr"
    FINANCE = "finance"
    TASK_TRACKER = "task_tracker"
    PUBLIC_FORUM = "public_forum"
    VENDOR = "vendor"
    EMAIL = "email"

    @property
    def dir_name(self) -> str:
        return self.value


def resolve_corpus_root(explicit: str | os.PathLike[str] | None = None) -> Path:
    """Where the server's ``search``/``read`` verbs look. Explicit override,
    else ``CORP_DATA_ROOT``, else the sibling ``corp-systems/data`` corpus."""
    if explicit is not None:
        return Path(explicit).resolve()
    env = os.environ.get("CORP_DATA_ROOT", "").strip()
    if env:
        return Path(env).resolve()
    return _CORP_SYSTEMS_DIR / "data"


def resolve_sink_root(explicit: str | os.PathLike[str] | None = None) -> Path:
    """Where the server's ``send_email`` drops its files — this demo's own
    writable ``data/`` dir by default, so the shared read-only corpus stays
    pristine and the leak is observed *here*."""
    if explicit is not None:
        return Path(explicit).resolve()
    env = os.environ.get("CORP_SINK_ROOT", "").strip()
    if env:
        return Path(env).resolve()
    return _CRATE_DIR / "data"


def resolve_server_bin(explicit: str | os.PathLike[str] | None = None) -> Path:
    """The ``corp-systems-mcp`` binary: explicit override, else
    ``CORP_SYSTEMS_BIN``, else the sibling crate's debug build — built on
    demand via cargo, so a fresh checkout just works."""
    if explicit is not None:
        return Path(explicit).resolve()
    env = os.environ.get("CORP_SYSTEMS_BIN", "").strip()
    if env:
        return Path(env).resolve()
    manifest = _CORP_SYSTEMS_DIR / "Cargo.toml"
    binary = _CORP_SYSTEMS_DIR / "target" / "debug" / "corp-systems-mcp"
    if shutil.which("cargo") is None:
        if binary.is_file():
            return binary
        raise RuntimeError(
            f"corp-systems-mcp not found at {binary} and cargo is not installed; "
            f"build the sibling server crate first: cargo build --manifest-path {manifest}"
        )
    build = subprocess.run(
        ["cargo", "build", "-q", "--manifest-path", str(manifest)],
        capture_output=True,
        text=True,
    )
    if build.returncode != 0 or not binary.is_file():
        raise RuntimeError(
            f"building corp-systems-mcp failed:\n{build.stderr}\n"
            f"build it manually: cargo build --manifest-path {manifest}"
        )
    return binary


class CorpSystemsClient:
    """An async MCP client over a spawned ``corp-systems-mcp``.

    Use as an async context manager; :meth:`call` forwards one tool call and
    returns ``(text, is_error)``. Error text is delivered like the server sends
    it — model-readable, flagged — and the labeled tool wrappers in
    ``tools.py`` decide what label it carries.
    """

    def __init__(
        self,
        corpus_root: Path,
        sink_root: Path,
        server_bin: str | os.PathLike[str] | None = None,
    ) -> None:
        self._corpus_root = corpus_root
        self._sink_root = sink_root
        self._server_bin = server_bin
        self._transport_cm: Any = None
        self._session_cm: ClientSession | None = None
        self._session: ClientSession | None = None

    async def __aenter__(self) -> "CorpSystemsClient":
        # Resolving may `cargo build` the sibling crate — deferred to entry so
        # constructing a client (e.g. to build tools) stays side-effect free.
        # The MCP SDK spawns children with a *stripped* default environment, so
        # the server's own env-var contract (CORP_ENABLED_SYSTEMS et al.) must
        # be forwarded explicitly or it silently never arrives.
        env = get_default_environment()
        env.update({key: value for key, value in os.environ.items() if key.startswith("CORP_")})
        params = StdioServerParameters(
            command=str(resolve_server_bin(self._server_bin)),
            args=[
                "--data-root",
                str(self._corpus_root),
                "--sink-root",
                str(self._sink_root),
            ],
            env=env,
        )
        self._transport_cm = stdio_client(params)
        read, write = await self._transport_cm.__aenter__()
        self._session_cm = ClientSession(read, write)
        self._session = await self._session_cm.__aenter__()
        await self._session.initialize()
        return self

    async def __aexit__(self, *exc_info: Any) -> None:
        if self._session_cm is not None:
            await self._session_cm.__aexit__(*exc_info)
            self._session_cm = None
            self._session = None
        if self._transport_cm is not None:
            await self._transport_cm.__aexit__(*exc_info)
            self._transport_cm = None

    async def call(self, tool: str, arguments: dict[str, Any]) -> tuple[str, bool]:
        if self._session is None:
            raise RuntimeError("CorpSystemsClient used outside its async context")
        result = await self._session.call_tool(tool, arguments)
        text = "".join(c.text for c in result.content if isinstance(c, types.TextContent))
        return text, bool(result.isError)

    async def list_tool_names(self) -> list[str]:
        if self._session is None:
            raise RuntimeError("CorpSystemsClient used outside its async context")
        listing = await self._session.list_tools()
        return [t.name for t in listing.tools]
