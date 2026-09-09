"""Invocation-scoped MCP evidence, using kagent's existing authenticated tools.

The host owns transports, credentials, approvals and execution. This toolset
enumerates metadata and checks policy coverage before exposing callable tools.
"""

from __future__ import annotations

import asyncio
import logging
import weakref
from dataclasses import dataclass, field

from google.adk.tools.base_toolset import BaseToolset
from google.adk.tools.mcp_tool.mcp_tool import McpTool

from . import wire
from .config_guard import ConfigRefused
from .discovery import MAX_BYTES, MAX_TOOLS, Discovery, discover_toolset
from .inventory import ToolInventory, mcp_source_id, mcp_spelling

logger = logging.getLogger(__name__)


@dataclass
class _Run:
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)
    opened: bool = False
    evidence: dict = field(default_factory=lambda: {"tools": [], "sources": []})
    selected: list = field(default_factory=list)
    # Keep identities for tool handles still held by the host, without retaining
    # every wrapper created by earlier model requests for the whole invocation.
    identities: weakref.WeakKeyDictionary = field(default_factory=weakref.WeakKeyDictionary)
    observations: list = field(default_factory=list)
    source_tools: list = field(default_factory=list)
    names: ToolInventory = field(default_factory=lambda: ToolInventory({}))


class MCPDiscovery(BaseToolset):
    def __init__(self, sources, plugin):
        super().__init__()
        self._use_invocation_cache = False
        self.sources = tuple(sources)
        self.plugin = plugin
        self.runs: dict[str, _Run] = {}
        self.servers = tuple(mcp_source_id(source._connection_params.url) for source in sources)
        if len(set(self.servers)) != len(self.servers):
            raise ConfigRefused("MCP endpoint is configured more than once; combine its filters and approvals")
        for source in sources:
            if source.tool_name_prefix or source._use_mcp_resources:
                raise ConfigRefused("MCP discovery requires native tool names and an explicit resource-tool identity")

    def state(self, invocation_id):
        if invocation_id not in self.runs:
            self.runs[invocation_id] = _Run(
                observations=[Discovery((), "unavailable") for _ in self.sources],
                source_tools=[[] for _ in self.sources],
            )
        return self.runs[invocation_id]

    async def _validate(self, root, child, inventory, *, pinned):
        try:
            return await asyncio.wait_for(self._request_validation(root, child, inventory, pinned=pinned), timeout=120)
        except asyncio.TimeoutError:
            raise ConfigRefused("MCP inventory validation timed out") from None

    async def _request_validation(self, root, child, inventory, *, pinned):
        request = {"protocol": wire.PROTOCOL, "adapter": wire.ADAPTER, "inventory": inventory}
        if pinned:
            request["root_id"] = root
            if child is not None:
                request["child_id"] = child
        try:
            async with self.plugin._live_client().stream(
                "POST", self.plugin._hook_url.removesuffix("/hook") + "/validate", json=request
            ) as response:
                if response.status_code == 404 and pinned:
                    return None
                if response.status_code != 200:
                    raise ConfigRefused(f"MCP inventory validation returned HTTP {response.status_code}")
                data = bytearray()
                async for chunk in response.aiter_bytes():
                    data.extend(chunk)
                    if len(data) > MAX_BYTES:
                        raise ConfigRefused("MCP validation response exceeds its size limit")
            import json

            report = json.loads(data)
        except ConfigRefused:
            raise
        except Exception:
            raise ConfigRefused("MCP inventory validation is unavailable or malformed") from None
        if not isinstance(report, dict) or any(
            not isinstance(report.get(key), list) for key in ("tools", "errors", "accepted_tools")
        ):
            raise ConfigRefused("MCP inventory validation response is incomplete")
        if not isinstance(report.get("actor_opened", False), bool):
            raise ConfigRefused("MCP inventory validation contains an invalid actor state")
        checked = {}
        for check in report["tools"]:
            if (
                not isinstance(check, dict)
                or not isinstance(check.get("tool"), str)
                or check.get("status") not in ("valid", "invalid", "unknown")
            ):
                raise ConfigRefused("MCP inventory validation contains an invalid status")
            checked[check["tool"]] = check["status"]
        for observed in inventory["tools"]:
            if observed["tool"] != wire.CONTROL_TOOL and checked.get(observed["name"]) not in ("valid", "invalid"):
                raise ConfigRefused("MCP inventory validation omitted a known tool")
        return report

    def _convert(self, source, metadata):
        # Same constructor fields in pinned ADK 1.31.1 and 2.8.0. Keep the
        # original session manager, auth, confirmation and progress handling.
        candidate = McpTool(
            mcp_tool=metadata,
            mcp_session_manager=source._mcp_session_manager,
            auth_scheme=source._auth_scheme,
            auth_credential=source._auth_credential,
            require_confirmation=source._require_confirmation,
            header_provider=source._header_provider,
            progress_callback=getattr(source, "_progress_callback", None),
        )
        from kagent.adk._mcp_toolset import ConnectionSafeMcpTool

        return ConnectionSafeMcpTool(candidate)

    async def prepare(self, context, *, allow_new=False):
        invocation = context.invocation_id
        self.plugin._discovery_runs[invocation] = self
        state = self.state(invocation)
        root, child = self.plugin._ids(context)
        async with state.lock:
            empty = {"tools": [], "sources": []}
            previous = await self._validate(root, child, empty, pinned=True)
            pinned = previous is not None
            if not pinned:
                if not allow_new or child is not None:
                    raise ConfigRefused("the requested APPA family has not opened")
                previous = await self._validate(root, child, empty, pinned=False)
            elif previous.get("actor_opened", False):
                state.opened = True
            accepted = {}
            for observed in previous["accepted_tools"]:
                if not isinstance(observed, dict) or not all(
                    isinstance(observed.get(key), str) for key in ("name", "tool")
                ):
                    raise ConfigRefused("MCP validation returned an invalid accepted identity")
                accepted[observed["name"]] = observed["tool"]
            semaphore = asyncio.Semaphore(4)

            async def inspect(index, source):
                async with semaphore:
                    try:
                        observed = await discover_toolset(source, context)
                        if observed.status != "unavailable":
                            try:
                                candidates = [self._convert(source, metadata) for metadata in observed.tools]
                            except ValueError:
                                raise ConfigRefused("MCP tool metadata cannot be converted by the host") from None
                            if observed.status == "partial":
                                retained = {tool.name: tool for tool in state.source_tools[index]}
                                retained.update({tool.name: tool for tool in candidates})
                                if len(retained) > MAX_TOOLS:
                                    raise ConfigRefused("MCP partial inventory exceeds its tool limit")
                                candidates = list(retained.values())
                            state.source_tools[index] = candidates
                        state.observations[index] = observed
                    except ConfigRefused:
                        if not state.opened:
                            raise
                        state.observations[index] = Discovery(
                            (), "unavailable", "invalid MCP metadata update; previous tools retained"
                        )
                        logger.warning(
                            "MCP source %s metadata update refused; previous tools retained", self.servers[index]
                        )

            results = await asyncio.gather(
                *(inspect(i, source) for i, source in enumerate(self.sources)), return_exceptions=True
            )
            for result in results:
                if isinstance(result, BaseException):
                    raise result
            base = self.plugin._inventory
            choices = {}
            for server, tools in zip(self.servers, state.source_tools):
                for candidate in tools:
                    choices.setdefault(candidate.name, []).append((candidate, mcp_spelling(server, candidate.name)))
            selected = {}
            for name, candidates in choices.items():
                eligible = [
                    (tool, identity)
                    for tool, identity in candidates
                    if name not in base.spellings and accepted.get(name, identity) == identity
                ]
                if len(eligible) != 1:
                    if not state.opened:
                        raise ConfigRefused(
                            f"MCP tool name {name!r} is ambiguous or conflicts with an existing identity"
                        )
                    continue
                selected[name] = eligible[0]
            names = dict(base.spellings)
            names.update({name: identity for name, (_, identity) in selected.items()})
            inventory = {
                "tools": [{"name": name, "tool": identity} for name, identity in sorted(names.items())],
                "sources": [
                    {
                        "server": server,
                        "status": observed.status,
                        "dynamic": True,
                        **({"detail": observed.detail} if observed.detail else {}),
                    }
                    for server, observed in zip(self.servers, state.observations)
                ],
            }
            report = await self._validate(root, child, inventory, pinned=pinned)
            if report is None or report["errors"]:
                raise ConfigRefused("MCP inventory configuration is invalid")
            invalid = {check["tool"] for check in report["tools"] if check["status"] == "invalid"}
            # Missing policy coverage disables a tool, not the other tools in
            # this conversation. Only covered identities enter the gated inventory.
            inventory["tools"] = [entry for entry in inventory["tools"] if entry["name"] not in invalid]
            state.names = ToolInventory({entry["name"]: entry["tool"] for entry in inventory["tools"]})
            state.selected = [tool for name, (tool, _) in sorted(selected.items()) if name not in invalid]
            for name, (tool, identity) in selected.items():
                if name not in invalid:
                    state.identities[tool] = identity
            state.evidence = inventory
            return state

    async def get_tools(self, readonly_context=None):
        if readonly_context is None:
            raise ConfigRefused("MCP discovery requires an invocation scope")
        return (await self.prepare(readonly_context)).selected

    async def close(self):
        try:
            results = await asyncio.gather(*(source.close() for source in self.sources), return_exceptions=True)
            for result in results:
                if isinstance(result, BaseException):
                    raise result
        finally:
            for invocation in self.runs:
                if self.plugin._discovery_runs.get(invocation) is self:
                    self.plugin._discovery_runs.pop(invocation)
            self.runs.clear()
