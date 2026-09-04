"""The tool inventory: every name ADK can dispatch on this agent, and its spelling on the wire.

The plugin gates a tool under a structured spelling, never under the
bare name ADK dispatches it by: an MCP tool as ``mcp:<toolset>/<tool>``,
a remote agent as ``agent:<namespace>/<agent>``, a tool the kagent
runtime attaches itself as ``builtin:<name>``, an out-of-band flow the
entrypoint gates as ``gate:<name>``, and the runtime's own control tool
as ``appa:execute_remedy_plan``. The runtime derives the canonical tool
and whether the call is a spawn from that spelling.

The entrypoint builds the inventory once at startup from the rendered
config, so what the wire can name is fixed before the model runs.
A call of a name outside it is refused at the gate, never forwarded.

The inverse travels with it. The runtime names a tool back to the model
by the spelling it received, which is not a name the model can call, so
``despell`` spells it into the name ADK dispatches. The builder owns
both directions and refuses a config whose two raw names spell alike,
so every spelling the wire carries names one tool the model can call.

- An MCP entry (``http_tools``, ``sse_tools``) names its tools in its
  ``tools`` filter, and a gated agent must carry one: without it the
  server decides the tool list at runtime, and the gate cannot name
  what it did not see. The toolset is the first DNS label of the server
  host in ``params.url``, the name the RemoteMCPServer resource carries
  in the cluster.
- kagent renders a remote agent's tool name as
  ``<namespace>__NS__<agent>`` with hyphens as underscores. Both halves
  are DNS-1123 labels, which carry no underscore, so the real names
  come back exactly. The rendering is not injective over every name a
  config can carry — ``team_a__NS__x`` and ``team-a__NS__x`` spell
  alike — and the builder refuses the pair rather than lose one.
- The builtins come from ``builtins.json``, the manifest pinned to the
  kagent-adk version this image wraps, in groups the rendered config
  and the runtime's environment switch on.
"""

from __future__ import annotations

import json
import os
import re
from collections.abc import Mapping
from dataclasses import dataclass
from importlib import resources
from typing import Any
from urllib.parse import urlsplit

from . import wire
from .config_guard import ConfigRefused

LANE = "python"
"""This image's key in the shared builtin manifest."""

SKILLS_FOLDER_ENV = "KAGENT_SKILLS_FOLDER"
"""The kagent runtime attaches its skills tools while this names a directory."""

_MCP_KEYS = ("http_tools", "sse_tools")
_NAMESPACE_MARK = "__NS__"
# One segment of a canonical tool id, as the runtime admits it.
_SEGMENT = re.compile(r"^[A-Za-z0-9_.-]+$")
# A wire spelling as it stands in a runtime string: two or three
# segments, ``class:name`` or ``class:namespace/name``. Each run of
# segment characters is maximal, so a spelling inside a longer run is
# no candidate here, and the inventory alone decides which candidate is
# a spelling it gave out.
_SPELLED = re.compile(r"(?<![A-Za-z0-9_.:/-])[A-Za-z0-9_.-]+:[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)?")


def mcp_spelling(toolset: str, tool: str) -> str:
    return f"mcp:{toolset}/{tool}"


def agent_spelling(namespace: str, agent: str) -> str:
    return f"agent:{namespace}/{agent}"


def builtin_spelling(name: str) -> str:
    return f"builtin:{name}"


def gate_spelling(name: str) -> str:
    return f"gate:{name}"


def is_spawn(spelling: str) -> bool:
    """Whether a spelled tool runs another agent: the ``agent:`` class."""
    return spelling.startswith("agent:")


def builtin_manifest() -> dict[str, Any]:
    """The packaged builtin manifest, every lane included."""
    return json.loads(resources.files(__package__).joinpath("builtins.json").read_text())


@dataclass(frozen=True)
class ToolInventory:
    """The raw ADK tool names of one agent, each with its wire spelling."""

    spellings: Mapping[str, str]
    names: Mapping[str, str]
    """The inverse: each wire spelling, back to the name ADK dispatches.

    ``from_config`` is the only builder, and it fills both directions as
    one: a spelling is assigned to at most one raw name, so the inverse
    loses nothing.
    """

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
        """
        return _SPELLED.sub(lambda match: self.names.get(match.group(), match.group()), text)

    @classmethod
    def from_config(cls, config: Mapping[str, Any], environ: Mapping[str, str] = os.environ) -> ToolInventory:
        """Build the inventory of a rendered kagent config.

        Raises ``ConfigRefused`` for an MCP entry without a tool filter,
        a name the wire cannot spell, a raw name declared twice, and two
        raw names that spell alike.
        """
        builder = _Builder()
        builder.add(wire.RESERVED_TOOL, wire.CONTROL_TOOL, "the reserved tool")
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
        return cls(dict(builder.spellings), dict(builder.names))


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
        toolset = _toolset_of(url if isinstance(url, str) else "")
        # A doubled underscore is the mark kagent reserves, so the runtime
        # admits no canonical id whose namespace carries one.
        if toolset is None or "__" in toolset:
            raise ConfigRefused(
                f"{path}: the toolset name is the first label of the server host in params.url, and "
                f"{url!r} carries none the wire can spell"
            )
        names = server.get("tools")
        if not isinstance(names, list) or not names or not all(isinstance(name, str) for name in names):
            raise ConfigRefused(
                f"{path} declares no tool filter, and the gate names only what the config declares — "
                "list under `tools` every tool of this server the agent may call"
            )
        for position, name in enumerate(names):
            if not _SEGMENT.match(name):
                raise ConfigRefused(
                    f"{path}.tools.{position}: the tool name {name!r} is outside what the wire can spell"
                )
            self.add(name, mcp_spelling(toolset, name), path)

    def remote_agent(self, path: str, remote: dict[str, Any]) -> None:
        name = remote.get("name")
        if not isinstance(name, str):
            return
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


def _toolset_of(url: str) -> str | None:
    try:
        host = urlsplit(url).hostname
    except ValueError:
        return None
    if not host:
        return None
    label = host.split(".", 1)[0]
    return label if _SEGMENT.match(label) else None
