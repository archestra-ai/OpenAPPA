import asyncio
import sys

import pytest
from mcp.types import ListToolsResult, Tool

from appa_kagent_adk.config_guard import ConfigRefused
from appa_kagent_adk.discovery import MAX_BYTES, MAX_TOOLS, discover, discover_toolset


def page(*names, cursor=None):
    return ListToolsResult(tools=[Tool(name=name, inputSchema={"type": "object"}) for name in names], nextCursor=cursor)


class Session:
    def __init__(self, pages):
        self.pages = iter(pages)
        self.cursors = []

    async def list_tools(self, *, cursor=None):
        self.cursors.append(cursor)
        result = next(self.pages)
        if isinstance(result, BaseException):
            raise result
        return result


@pytest.mark.asyncio
async def test_all_pages_are_discovered_and_filter_is_optional():
    session = Session([page("write", cursor="next"), page("read")])
    result = await discover(session)
    assert result.status == "complete"
    assert [tool.name for tool in result.tools] == ["read", "write"]
    assert session.cursors == [None, "next"]
    selected = await discover(Session([page("write", cursor="next"), page("read")]), ["read"])
    assert [tool.name for tool in selected.tools] == ["read"]


@pytest.mark.asyncio
async def test_transport_failure_is_not_an_empty_complete_listing_and_is_redacted():
    error = RuntimeError("secret-auth-token")
    unavailable = await discover(Session([error]))
    assert unavailable.status == "unavailable"
    partial = await discover(Session([page("read", cursor="next"), error]))
    assert partial.status == "partial"
    assert [tool.name for tool in partial.tools] == ["read"]
    assert "secret-auth-token" not in repr(partial)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "pages",
    [
        [page("invalid/name")],
        [page("")],
        [page("read", cursor="next"), page("read")],
        [page("read", cursor="next"), page("write", cursor="next")],
    ],
)
async def test_ambiguous_or_cyclic_listings_are_known_errors(pages):
    with pytest.raises(ConfigRefused):
        await discover(Session(pages))


@pytest.mark.asyncio
async def test_missing_filter_entry_is_an_error_only_after_complete_discovery():
    with pytest.raises(ConfigRefused):
        await discover(Session([page("read")]), ["missing"])
    result = await discover(Session([RuntimeError("unreachable")]), ["missing"])
    assert result.status == "unavailable"


@pytest.mark.asyncio
async def test_cancellation_propagates():
    with pytest.raises(asyncio.CancelledError):
        await discover(Session([asyncio.CancelledError()]))


@pytest.mark.asyncio
async def test_metadata_count_and_byte_limits_apply_before_filtering():
    oversized = ListToolsResult(tools=[Tool(name="read", description="x" * MAX_BYTES, inputSchema={})])
    with pytest.raises(ConfigRefused):
        await discover(Session([oversized]), ["other"])
    too_many = page(*(f"tool_{index}" for index in range(MAX_TOOLS + 1)))
    with pytest.raises(ConfigRefused):
        await discover(Session([too_many]), ["tool_0"])


_SERVER = """
import asyncio
from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import ListToolsRequest, ListToolsResult, Tool
server = Server('discovery-test')
@server.list_tools()
async def tools(request: ListToolsRequest):
    second = request.params and request.params.cursor
    return ListToolsResult(
        tools=[Tool(name='read' if second else 'write', inputSchema={'type': 'object'})],
        nextCursor=None if second else 'second',
    )
@server.call_tool()
async def call(name, arguments):
    raise AssertionError('discovery must never call a tool')
async def main():
    async with stdio_server() as (reader, writer):
        await server.run(reader, writer, server.create_initialization_options())
asyncio.run(main())
"""


@pytest.mark.asyncio
async def test_stock_toolset_session_discovers_all_pages_and_keeps_known_errors():
    from google.adk.tools.mcp_tool.mcp_toolset import McpToolset
    from mcp import StdioServerParameters

    toolset = McpToolset(
        connection_params=StdioServerParameters(command=sys.executable, args=["-c", _SERVER]),
        tool_filter=["read"],
    )
    try:
        result = await discover_toolset(toolset)
        assert result.status == "complete"
        assert [tool.name for tool in result.tools] == ["read"]
        toolset.tool_filter = ["missing"]
        with pytest.raises(ConfigRefused):
            await discover_toolset(toolset)
    finally:
        await toolset.close()
