import os
import socket
import subprocess
import sys
import time

import httpx
import pytest
from conftest import FakeContent, FakeInvocationContext, FakeSession
from google.adk.agents.readonly_context import ReadonlyContext
from google.adk.tools.tool_context import ToolContext

from appa_kagent_adk.config_guard import ConfigRefused
from appa_kagent_adk.inventory import ToolInventory
from appa_kagent_adk.mcp_lifecycle import MCPDiscovery
from appa_kagent_adk.plugin import AppaPluginKagent


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def start(tmp_path, name, args, url):
    log = (tmp_path / f"{name}.log").open("w+")
    process = subprocess.Popen(args, stdout=log, stderr=log)
    try:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                with httpx.Client(timeout=0.2) as client:
                    client.get(url)
                return process, log
            except httpx.TransportError:
                if process.poll() is not None:
                    break
                time.sleep(0.03)
        log.seek(0)
        pytest.fail(f"{name} did not start: {log.read()}")
    except BaseException:
        process.terminate()
        process.wait(timeout=5)
        log.close()
        raise


SERVER = """
import pathlib, sys
from mcp.server.fastmcp import FastMCP
from mcp.types import ListToolsRequest, ListToolsResult, Tool, TextContent
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.responses import Response
import uvicorn
generation, calls = map(pathlib.Path, sys.argv[2:4])
server = FastMCP('fixture')
@server._mcp_server.list_tools()
async def listing(request: ListToolsRequest):
    second = request.params and request.params.cursor
    if second and generation.read_text() == '3': raise RuntimeError('fixture page unavailable')
    names = ['read'] if not second else ([] if generation.read_text() == '0' else ['late', 'uncovered'])
    if generation.read_text() == '2': names = ['read', 'read']
    return ListToolsResult(
        tools=[Tool(name=name, inputSchema={'type': 'object'}) for name in names],
        nextCursor=None if second else 'next',
    )
@server._mcp_server.call_tool()
async def call(name, arguments):
    calls.write_text(calls.read_text() + name + '\\n')
    return [TextContent(type='text', text='original result bytes')]
class Auth(BaseHTTPMiddleware):
    async def dispatch(self, request, call_next):
        if request.headers.get('authorization') != 'fixture-token': return Response(status_code=401)
        return await call_next(request)
app = server.streamable_http_app()
app.add_middleware(Auth)
uvicorn.run(app, host='127.0.0.1', port=int(sys.argv[1]), log_level='error')
"""


async def test_authenticated_discovery_late_tools_and_restart_against_runtime(tmp_path):
    kagent = pytest.importorskip("kagent.adk._mcp_toolset")
    from google.adk.tools.mcp_tool.mcp_session_manager import StreamableHTTPConnectionParams

    binary = os.environ.get("APPA_TEST_RUNTIME_BIN")
    if not binary:
        pytest.skip("set APPA_TEST_RUNTIME_BIN for the real runtime regression")
    config = tmp_path / "appa.toml"
    config.write_text(
        "[externals]\ntimeout_ms=30000\nmax_body_bytes=65536\n[policy]\nversion=2\n[[policy.tool]]\nname='read'\n[[policy.tool]]\nname='late'\n"
    )
    generation, calls = tmp_path / "generation", tmp_path / "calls"
    generation.write_text("0")
    calls.write_text("")
    runtime_port, mcp_port = port(), port()
    runtime_url = f"http://127.0.0.1:{runtime_port}"
    endpoint = f"http://127.0.0.1:{mcp_port}/mcp"
    runtime, runtime_log = start(
        tmp_path,
        "runtime",
        [
            binary,
            "runtime",
            "--config",
            str(config),
            "--db",
            str(tmp_path / "appa.db"),
            "--adapter",
            "kagent",
            "--listen",
            f"127.0.0.1:{runtime_port}",
        ],
        runtime_url + "/health",
    )
    server, server_log = start(
        tmp_path, "mcp", [sys.executable, "-c", SERVER, str(mcp_port), str(generation), str(calls)], endpoint
    )
    opened = []

    def host(root):
        plugin = AppaPluginKagent(runtime_url, inventory=ToolInventory({}))
        source = kagent.KAgentMcpToolset(
            connection_params=StreamableHTTPConnectionParams(url=endpoint, headers={"Authorization": "fixture-token"})
        )
        discovery = MCPDiscovery([source], plugin)
        context = FakeInvocationContext(FakeSession(root))
        context.agent.tools = [discovery]
        opened.append((plugin, discovery))
        return plugin, discovery, context

    try:
        plugin, discovery, context = host("python-discovery")
        await plugin.on_user_message_callback(invocation_context=context, user_message=FakeContent("read"))
        tools = await discovery.get_tools(ReadonlyContext(context))
        assert [tool.name for tool in tools] == ["read"]
        assert calls.read_text() == ""
        generation.write_text("1")
        tools = await discovery.get_tools(ReadonlyContext(context))
        assert [tool.name for tool in tools] == ["late", "read"]
        candidate = tools[0]
        tool_context = ToolContext(context, function_call_id="late-call")
        assert await plugin.before_tool_callback(tool=candidate, tool_args={}, tool_context=tool_context) is None
        result = await candidate.run_async(args={}, tool_context=tool_context)
        assert "original result bytes" in str(result)
        await plugin.after_tool_callback(tool=candidate, tool_args={}, tool_context=tool_context, result=result)
        assert calls.read_text() == "late\n"
        generation.write_text("2")
        assert [tool.name for tool in await discovery.get_tools(ReadonlyContext(context))] == ["late", "read"]
        generation.write_text("3")
        assert [tool.name for tool in await discovery.get_tools(ReadonlyContext(context))] == ["late", "read"]
        generation.write_text("1")
        plugin, discovery, context = host("python-discovery")
        await plugin.on_user_message_callback(invocation_context=context, user_message=FakeContent("continue"))
        assert [tool.name for tool in await discovery.get_tools(ReadonlyContext(context))] == ["late", "read"]
        plugin, discovery, context = host("python-invalid")
        with pytest.raises(ConfigRefused):
            await plugin.on_user_message_callback(invocation_context=context, user_message=FakeContent("read"))
        assert calls.read_text() == "late\n"
    finally:
        for plugin, discovery in opened:
            await discovery.close()
            await plugin.close()
        for process, log in ((server, server_log), (runtime, runtime_log)):
            process.terminate()
            process.wait(timeout=5)
            log.close()
