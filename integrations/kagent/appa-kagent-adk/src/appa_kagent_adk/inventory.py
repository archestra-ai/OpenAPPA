"""Configured names and wire identities; MCP discovery supplies observed tools.

Optional MCP filters are not permission. The host validates authenticated metadata
before model exposure and the runtime checks every call. Full endpoint identities
never infer a provider from a hostname. Remote-agent and builtin mappings retain
kagent\'s native conventions. The inverse renders guidance in host-native names.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import string
from collections.abc import Mapping
from dataclasses import dataclass, field
from importlib import resources
from types import MappingProxyType
from typing import Any
from urllib.parse import urlsplit

from . import wire
from .config_guard import ConfigRefused

LANE = "python"
"""This image's key in the shared builtin manifest."""

SKILLS_FOLDER_ENV = "KAGENT_SKILLS_FOLDER"
"""The kagent runtime attaches its skills tools while this names a directory."""

GUIDE_ENV = "APPA_GUIDE"
GUIDE_TOOLSET = "appa-guide"
"""appa-guide's management set is served over its own endpoint and named
under one fixed toolset, so a policy names it the way it names any other
MCP tool."""

_MCP_KEYS = ("http_tools", "sse_tools")
_NAMESPACE_MARK = "__NS__"
# The characters an identifier is built of. A core character always
# continues one; a separator continues one only where a core character
# follows it, so the period that ends a sentence closes the identifier
# and the period inside ``list.json`` does not.
_CORE = frozenset(string.ascii_letters + string.digits + "_-")
_SEPARATOR = frozenset(".:/")
# One segment of a wire spelling: a run that starts and ends on a core
# character and is maximal over them.
_SEGMENT_RUN = r"[A-Za-z0-9_-](?:[A-Za-z0-9_.-]*[A-Za-z0-9_-])?"
# One segment of a canonical tool id, as the runtime admits it. It is
# the run's own grammar anchored, so every name the inventory accepts
# is a name ``despell`` can match back: a boundary period would end the
# run short, and the spelling could never be found whole.
_SEGMENT = re.compile(rf"^{_SEGMENT_RUN}$")
# The classes a wire spelling begins with. Every spelling this module
# issues carries one, so the scan below anchors on the literal class
# instead of trying a segment at every character: an unanchored scan
# backtracks over each identifier it fails to close, which is quadratic
# in one long colon-free run — and a tool result is enough to carry one.
_CLASSES = ("mcp", "agent", "builtin", "gate", wire.CONTROL_TOOL.split(":", 1)[0])
# A wire spelling as it stands in a runtime string: two or three
# segments, ``class:name`` or ``class:namespace/name``. The inventory
# alone decides which candidate is a spelling it gave out, and
# ``despell`` replaces one only where the identifier continues on
# neither side.
_SPELLED = re.compile(rf"(?:{'|'.join(_CLASSES)}):{_SEGMENT_RUN}(?:/{_SEGMENT_RUN})?")


def _char(text: str, index: int) -> str:
    """The character at ``index``, or the empty string outside ``text``."""
    return text[index] if 0 <= index < len(text) else ""


def _continues(adjacent: str, beyond: str) -> bool:
    """Whether an identifier runs on past one of its edges.

    ``adjacent`` is the character next to the candidate and ``beyond``
    the one past it, read away from the candidate; both are empty at
    the ends of the text.
    """
    return adjacent in _CORE or (adjacent in _SEPARATOR and beyond in _CORE)


def mcp_source_id(endpoint: str) -> str:
    try:
        parsed = urlsplit(endpoint)
        valid = (
            isinstance(endpoint, str)
            and parsed.scheme in ("http", "https")
            and parsed.hostname
            and not parsed.username
            and not parsed.password
            and not parsed.fragment
        )
        _ = parsed.port
    except (TypeError, ValueError):
        valid = False
    if not valid:
        raise ConfigRefused("MCP endpoint must be an HTTP(S) URL without userinfo or a fragment")
    return "server-" + hashlib.sha256(endpoint.encode("utf-8")).hexdigest()


def mcp_spelling(toolset: str, tool: str) -> str:
    return f"mcp:{toolset}/{tool}"


def agent_spelling(namespace: str, agent: str) -> str:
    return f"agent:{namespace}/{agent}"


def builtin_spelling(name: str) -> str:
    return f"builtin:{name}"


def gate_spelling(name: str) -> str:
    return f"gate:{name}"


def guide_spelling(name: str) -> str:
    return mcp_spelling(GUIDE_TOOLSET, name)


def guide_enabled(environ: Mapping[str, str] = os.environ) -> bool:
    """Whether this deployment is appa-guide's own agent.

    The inventory, which spells the management set only for the guide,
    and the toolset the guide reaches it through read the same answer,
    and a value that is neither refuses the config rather than start an
    agent that silently has no guide.
    """
    match environ.get(GUIDE_ENV, "").strip().lower():
        case "" | "false":
            return False
        case "true":
            return True
        case other:
            raise ConfigRefused(f"{GUIDE_ENV} must be true or false, not {other!r}")


def is_spawn(spelling: str) -> bool:
    """Whether a spelled tool runs another agent: the ``agent:`` class."""
    return spelling.startswith("agent:")


def builtin_manifest() -> dict[str, Any]:
    """The packaged builtin manifest, every lane included."""
    return json.loads(resources.files(__package__).joinpath("builtins.json").read_text())


@dataclass(frozen=True)
class ToolInventory:
    """The raw ADK tool names of one agent, each with its wire spelling.

    The forward direction is the whole state. The inverse is derived,
    never given, so no caller can hand in two mappings that disagree,
    and both are handed out as read-only views. A forward mapping that
    is not injective has no inverse — the runtime could name only one
    of the two raw names back to the model — and it is refused here as
    ``from_config`` refuses it at the config.
    """

    spellings: Mapping[str, str]
    names: Mapping[str, str] = field(init=False)
    """The inverse: each wire spelling, back to the name ADK dispatches."""

    def __post_init__(self) -> None:
        spellings = dict(self.spellings)
        names = {spelling: name for name, spelling in spellings.items()}
        if len(names) != len(spellings):
            raise ValueError("two raw names of the inventory spell alike, and the inverse would lose one")
        object.__setattr__(self, "spellings", MappingProxyType(spellings))
        object.__setattr__(self, "names", MappingProxyType(names))

    def spelling(self, name: str) -> str | None:
        """The wire spelling of a raw name, or None for a name outside the inventory."""
        return self.spellings.get(name)

    def despell(self, text: str) -> str:
        """``text`` with every wire spelling of this inventory replaced by the name ADK dispatches.

        The runtime names a tool by the spelling it was given, and the
        model dispatches another name, so runtime text that reaches the
        model passes through here first. The substitution is closed: a
        whole spelling this inventory carries is replaced, and every
        other byte stands — a spelling it never gave out included.

        A whole spelling is one the identifier continues on neither
        side, so ``mcp:demo/list`` inside ``mcp:demo/list/extra`` names
        no tool this inventory gave out and stands, while the period
        that ends the sentence after one is punctuation and is kept.
        """

        def replace(match: re.Match[str]) -> str:
            start, end = match.start(), match.end()
            if _continues(_char(text, start - 1), _char(text, start - 2)):
                return match.group()
            if _continues(_char(text, end), _char(text, end + 1)):
                return match.group()
            return self.names.get(match.group(), match.group())

        return _SPELLED.sub(replace, text)

    @classmethod
    def from_config(cls, config: Mapping[str, Any], environ: Mapping[str, str] = os.environ) -> ToolInventory:
        """Build the inventory of a rendered kagent config.

        Raises ``ConfigRefused`` for an invalid configured endpoint,
        a name the wire cannot spell, a raw name declared twice, and two
        raw names that spell alike.
        """
        builder = _Builder()
        builder.add(wire.RESERVED_TOOL, wire.CONTROL_TOOL, "the reserved tool")
        if guide_enabled(environ):
            for name in sorted(wire.MANAGEMENT_TOOLS):
                builder.add(name, guide_spelling(name), "the appa-guide management set")
        for key in _MCP_KEYS:
            for index, server in enumerate(_entries(config.get(key))):
                builder.mcp_server(f"{key}.{index}", server)
        for index, remote in enumerate(_entries(config.get("remote_agents"))):
            builder.remote_agent(f"remote_agents.{index}", remote)
        enabled = {
            "always": True,
            "memory": config.get("memory") is not None,
            "skills": bool(environ.get(SKILLS_FOLDER_ENV, "").strip()),
        }
        for group, names in builtin_manifest()[LANE]["groups"].items():
            if group not in enabled:
                raise ValueError(f"the builtin manifest names a group this image does not switch on: {group}")
            if enabled[group]:
                for name in names:
                    builder.add(name, builtin_spelling(name), f"the builtin group {group}")
        return cls(builder.spellings)


def _entries(raw: Any) -> list[dict[str, Any]]:
    """The dict entries of a raw config list.

    The schema validation the entrypoint ran already refused any other shape.
    """
    if not isinstance(raw, list):
        return []
    return [entry for entry in raw if isinstance(entry, dict)]


class _Builder:
    """Both directions of one inventory, each name and each spelling taken once."""

    def __init__(self) -> None:
        self.spellings: dict[str, str] = {}
        self.names: dict[str, str] = {}
        self._sources: dict[str, str] = {}

    def add(self, name: str, spelling: str, source: str) -> None:
        declared = self._sources.get(name)
        if declared is not None:
            raise ConfigRefused(
                f"the config declares the tool name {name!r} twice ({declared} and {source}), and the "
                "gate cannot tell the two apart — rename one of them"
            )
        spelled = self.names.get(spelling)
        if spelled is not None:
            raise ConfigRefused(
                f"the config declares {spelled!r} ({self._sources[spelled]}) and {name!r} ({source}), and "
                f"both spell as {spelling!r} on the wire — the runtime could name only one of them back "
                "to the model, so rename one of them"
            )
        self.spellings[name] = spelling
        self.names[spelling] = name
        self._sources[name] = source

    def mcp_server(self, path: str, server: dict[str, Any]) -> None:
        params = server.get("params")
        url = params.get("url") if isinstance(params, dict) else None
        try:
            toolset = mcp_source_id(url)
        except ConfigRefused as error:
            raise ConfigRefused(f"{path}: {error}") from None
        names = server.get("tools") or []
        if not isinstance(names, list) or not all(isinstance(name, str) for name in names):
            raise ConfigRefused(f"{path}.tools must be a list of tool names")
        for position, name in enumerate(names):
            if not _SEGMENT.match(name):
                raise ConfigRefused(
                    f"{path}.tools.{position}: the tool name {name!r} is outside what the wire can spell"
                )
            self.add(name, mcp_spelling(toolset, name), path)

    def remote_agent(self, path: str, remote: dict[str, Any]) -> None:
        """Spell one remote agent by the name the entry carries.

        The identity is the name alone, and the entry's ``url`` binds
        nothing — unlike an MCP entry, whose toolset is read off its own
        endpoint. The kagent controller renders both fields from the one
        Agent object a tool reference resolves to: the name from its
        object reference and the URL from `toolAgentURL` of that same
        object. The reference is a `TypedReference` (kind, name,
        namespace) and carries no URL, so no CRD can point a declared
        agent identity at another endpoint.

        Reading the identity off the URL instead would refuse two
        renderings the controller emits: a global proxy rewrites every
        URL to the proxy host and moves the real one into the
        ``x-kagent-host`` header, and a sandbox agent is reached at the
        controller's own address under `/api/a2a-sandboxes/<ns>/<name>`.
        A hand-written ``config.json`` mounted past the controller can
        still name one agent and reach another — the Known gaps table of
        integrations/kagent/IMPLEMENTATION.md.

        The ``url`` is not read at all here, where the go lane skips a
        url-less entry: this runtime wires every declared remote agent
        whatever its url (``kagent.adk.types.AgentConfig.to_agent``),
        and the go one skips the url-less ones, so each lane's
        inventory follows the tools its own runtime builds.
        """
        name = remote.get("name")
        if not isinstance(name, str):
            raise ConfigRefused(
                f"{path}.name: a remote agent is wired as a tool of its name, and this entry declares none"
            )
        namespace, mark, agent = name.partition(_NAMESPACE_MARK)
        if not mark or not namespace or not agent or _NAMESPACE_MARK in agent:
            raise ConfigRefused(
                f"{path}.name: kagent renders a remote agent as <namespace>__NS__<agent>, and {name!r} "
                "is not that shape"
            )
        namespace, agent = namespace.replace("_", "-"), agent.replace("_", "-")
        if not _SEGMENT.match(namespace) or not _SEGMENT.match(agent):
            raise ConfigRefused(f"{path}.name: the remote agent name {name!r} is outside what the wire can spell")
        self.add(name, agent_spelling(namespace, agent), path)
