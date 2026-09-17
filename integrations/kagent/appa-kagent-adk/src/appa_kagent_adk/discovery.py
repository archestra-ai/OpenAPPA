"""Bounded metadata discovery using the host's already-authenticated MCP session.

This module never opens a connection, invokes a tool, or grants policy coverage.
The host retains ownership of transports, credentials, and session cleanup.
"""

from __future__ import annotations

import asyncio
import re
from dataclasses import dataclass
from typing import Literal

from mcp import ClientSession
from mcp.types import Tool

from .config_guard import ConfigRefused

MAX_TOOLS = 10_000
MAX_BYTES = 10 * 1024 * 1024
TIMEOUT_SECONDS = 30


@dataclass(frozen=True)
class Discovery:
    tools: tuple[Tool, ...]
    status: Literal["complete", "partial", "unavailable"]
    detail: str | None = None


async def discover_toolset(toolset, readonly_context=None) -> Discovery:
    """Probe a stock toolset through its authenticated session boundary.

    The two pinned ADK lanes expose this same session operation. It retains
    kagent's TLS configuration, static headers, token propagation and OAuth.
    The outer deadline also bounds connection setup, not only list_tools.
    Validation errors return through the callback as data because ADK otherwise
    wraps them in ConnectionError, which would misreport known errors as unknown.
    """
    selected = toolset.tool_filter
    if selected is not None and not isinstance(selected, list):
        raise ConfigRefused("MCP discovery requires a name-list filter or no filter")

    async def inspect(session):
        try:
            return await discover(session, selected)
        except ConfigRefused as error:
            return error

    try:
        result = await asyncio.wait_for(
            toolset._execute_with_session(inspect, "MCP metadata discovery did not complete", readonly_context),
            timeout=TIMEOUT_SECONDS,
        )
    except Exception:
        return Discovery((), "unavailable", "MCP connection or metadata discovery did not complete")
    if isinstance(result, ConfigRefused):
        raise result
    return result


async def discover(session: ClientSession, tool_filter: list[str] | None = None) -> Discovery:
    """Enumerate every page before applying the host's optional name filter.

    A failed listing is incomplete evidence, not an empty complete inventory.
    Invalid listings raise ConfigRefused; exception messages from a transport are
    never copied into diagnostics because they can contain authentication data.
    """
    found: dict[str, Tool] = {}
    selected = set(tool_filter) if tool_filter else None
    size = 0

    async def pages() -> None:
        nonlocal size
        cursor: str | None = None
        cursors: set[str] = set()
        while True:
            page = await session.list_tools(cursor=cursor)
            size += len(page.model_dump_json().encode("utf-8"))
            if size > MAX_BYTES or len(found) + len(page.tools) > MAX_TOOLS:
                raise ConfigRefused("MCP tool listing exceeds the metadata discovery limit")
            for tool in page.tools:
                if re.fullmatch(r"[A-Za-z0-9_.-]+", tool.name) is None:
                    raise ConfigRefused("MCP tool listing contains an invalid tool name")
                if tool.name in found:
                    raise ConfigRefused("MCP tool listing contains a duplicate tool name")
                found[tool.name] = tool
            cursor = page.nextCursor
            if not cursor:
                return
            if cursor in cursors:
                raise ConfigRefused("MCP tool listing repeats a pagination cursor")
            cursors.add(cursor)

    def tools() -> tuple[Tool, ...]:
        return tuple(found[name] for name in sorted(found) if selected is None or name in selected)

    try:
        await asyncio.wait_for(pages(), timeout=TIMEOUT_SECONDS)
    except ConfigRefused:
        raise
    except Exception:
        # Cancellation deliberately propagates: it is not a connection failure.
        return Discovery(tools(), "partial" if found else "unavailable", "MCP metadata discovery did not complete")
    if selected is not None and selected.difference(found):
        raise ConfigRefused("MCP tool filter names tools absent from the complete listing")
    return Discovery(tools(), "complete")
