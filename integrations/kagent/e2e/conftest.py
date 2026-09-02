"""End-to-end fixtures: a live appa-runtime, the demo MCP server, and a
real ADK agent loop driven by a scripted model.

Each scenario runs the REAL path — `AppaPluginKagent` over the real
`/hook` wire, the `appa-adapter-kagent` codec inside a real
`appa-runtime` loaded with the example policy, and the demo tools over
a real MCP transport. Only the model is a stub, so the tool calls are
deterministic; every APPA decision along the way is real.

These tests need the kagent lane installed (google-adk + mcp) and the
compiled `appa` binary. They skip when either is absent, and are also
gated behind APPA_E2E=1 so a bare unit run does not spawn processes.
"""

from __future__ import annotations

import contextlib
import os
import shutil
import socket
import subprocess
import time
from collections.abc import AsyncGenerator
from pathlib import Path

import httpx
import pytest

pytest.importorskip("google.adk", reason="the kagent/ADK lane is not installed")
pytest.importorskip("mcp", reason="mcp is not installed")

if os.environ.get("APPA_E2E") != "1":
    pytest.skip("set APPA_E2E=1 to run the kagent end-to-end scenarios", allow_module_level=True)

from google.adk.agents import Agent  # noqa: E402
from google.adk.models.base_llm import BaseLlm  # noqa: E402
from google.adk.models.llm_response import LlmResponse  # noqa: E402
from google.adk.runners import InMemoryRunner  # noqa: E402
from google.adk.tools.mcp_tool.mcp_session_manager import StreamableHTTPConnectionParams  # noqa: E402
from google.adk.tools.mcp_tool.mcp_toolset import McpToolset  # noqa: E402
from google.genai import types  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[3]
EXAMPLE_POLICY = REPO_ROOT / "integrations" / "kagent" / "examples" / "kagent.appa.toml"
DEMO_TOOLS = REPO_ROOT / "integrations" / "kagent" / "demo" / "demo_tools.py"


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def _appa_binary() -> str:
    for candidate in (REPO_ROOT / "target" / "release" / "appa", REPO_ROOT / "target" / "debug" / "appa"):
        if candidate.exists():
            return str(candidate)
    found = shutil.which("appa")
    if found:
        return found
    pytest.skip("no compiled appa binary found (build with: cargo build -p appa)")


def _wait(url: str, timeout: float = 30.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        with contextlib.suppress(httpx.HTTPError):
            if httpx.get(url, timeout=2.0).status_code == 200:
                return
        time.sleep(0.3)
    raise RuntimeError(f"{url} did not become ready")


def _wait_tcp(host: str, port: int, timeout: float = 40.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        with contextlib.suppress(OSError), socket.create_connection((host, port), timeout=2.0):
            return
        time.sleep(0.3)
    raise RuntimeError(f"{host}:{port} did not open")


def _offer_id_in(llm_request) -> str | None:
    """The offer id APPA quoted in the most recent deny, if any.

    A deny dict flows back as a function response `{"result": feedback,
    "appa": "denied"}` — the feedback rides under "result", the key
    kagent's converters serialize — and the feedback quotes
    `execute_remedy_plan(offer_id: "…")`. A model that wants to take the
    remedy reads it from the conversation, the way a real model would.
    Only the live offer works: the runtime refuses a forged or stale id
    ("no live offer with this id exists"), and the plugin's ToolCall for
    `execute_remedy_plan` is the vouch that binds it before `/mcp`.
    """
    import re

    for content in reversed(getattr(llm_request, "contents", []) or []):
        for part in content.parts or []:
            response = getattr(part.function_response, "response", None) if part.function_response else None
            if isinstance(response, dict) and response.get("appa") == "denied":
                match = re.search(r'offer_id:\s*"([a-f0-9]+)"', str(response.get("result", "")))
                if match:
                    return match.group(1)
    return None


class ScriptedModel(BaseLlm):
    """A model that plays a fixed list of turns.

    Each turn is a tool call `{"tool", "args"}`, a final `{"text"}`, or
    `{"remedy": True}` — take the offer APPA last quoted by calling
    `execute_remedy_plan` with its id. The agent loop drives one model
    turn per step; tool results feed back as the model's next request,
    which a plain turn ignores.
    """

    model: str = "scripted"
    turns: list = []
    _cursor: int = 0

    async def generate_content_async(self, llm_request, stream: bool = False) -> AsyncGenerator[LlmResponse, None]:
        index = self._cursor
        self._cursor += 1
        turn = self.turns[index] if index < len(self.turns) else {"text": "done"}
        if turn.get("remedy"):
            offer = _offer_id_in(llm_request)
            part = types.Part(
                function_call=types.FunctionCall(name="execute_remedy_plan", args={"offer_id": offer or "missing"})
            )
        elif "text" in turn:
            part = types.Part(text=turn["text"])
        else:
            part = types.Part(
                function_call=types.FunctionCall(name=turn["tool"], args=turn.get("args", {}))
            )
        yield LlmResponse(content=types.Content(role="model", parts=[part]))


@pytest.fixture(scope="session")
def runtime_url() -> str:
    binary = _appa_binary()
    port = _free_port()
    db = Path(f"/tmp/appa-e2e-{port}.db")
    db.unlink(missing_ok=True)
    process = subprocess.Popen(
        [binary, "runtime", "--adapter", "kagent", "--config", str(EXAMPLE_POLICY),
         "--db", str(db), "--listen", f"127.0.0.1:{port}"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    url = f"http://127.0.0.1:{port}"
    try:
        _wait(f"{url}/health")
        yield url
    finally:
        process.terminate()
        with contextlib.suppress(subprocess.TimeoutExpired):
            process.wait(timeout=5)
        db.unlink(missing_ok=True)


@pytest.fixture(scope="session")
def demo_tools_url() -> str:
    port = _free_port()
    process = subprocess.Popen(
        ["uv", "run", "--with", "mcp>=1.25,<2", "python", str(DEMO_TOOLS), "--port", str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    url = f"http://127.0.0.1:{port}/mcp"
    try:
        _wait_tcp("127.0.0.1", port)
        yield url
    finally:
        process.terminate()
        with contextlib.suppress(subprocess.TimeoutExpired):
            process.wait(timeout=5)


@pytest.fixture()
def run_scenario(runtime_url, demo_tools_url):
    """Return a coroutine that runs one scripted scenario and yields the
    events the agent produced (tool responses the model saw included)."""
    from appa_kagent_adk.plugin import AppaPluginKagent

    async def run(turns: list) -> list:
        toolset = McpToolset(connection_params=StreamableHTTPConnectionParams(url=demo_tools_url))
        # The reserved-tool toolset, exactly as the entrypoint attaches
        # it — so a scripted `execute_remedy_plan` call runs the real
        # remedy path over /mcp.
        reserved = McpToolset(
            connection_params=StreamableHTTPConnectionParams(url=runtime_url.rstrip("/") + "/mcp", timeout=300.0),
            tool_filter=["execute_remedy_plan"],
        )
        agent = Agent(
            name="cluster_ops",
            model=ScriptedModel(turns=turns),
            description="cluster-ops demo agent",
            instruction="You operate a kubernetes cluster.",
            tools=[toolset, reserved],
        )
        plugin = AppaPluginKagent(runtime_url)
        runner = InMemoryRunner(agent, app_name="cluster-ops", plugins=[plugin])
        session = await runner.session_service.create_session(app_name="cluster-ops", user_id="op")
        events = []
        try:
            async for event in runner.run_async(
                user_id="op",
                session_id=session.id,
                new_message=types.Content(role="user", parts=[types.Part(text=turns[0].get("prompt", "go"))]),
            ):
                events.append(event)
        finally:
            await plugin.close()
            await runner.close()
        return events

    return run


def tool_responses(events: list) -> list[dict]:
    """The function-response payloads the model saw, in order."""
    payloads = []
    for event in events:
        for part in (event.content.parts if event.content else []):
            if part.function_response is not None:
                payloads.append(dict(part.function_response.response or {}))
    return payloads
