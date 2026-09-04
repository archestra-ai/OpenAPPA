"""Fakes and a scripted /hook transport for the plugin tests.

The plugin reads ADK objects by attribute, so plain fakes stand in for
sessions and contexts. The transport is httpx's MockTransport: every
posted event is recorded, and the scripted answers play back in order —
no network, no runtime.
"""

from __future__ import annotations

import json
from typing import Any, Protocol

import httpx

from appa_kagent_adk.inventory import ToolInventory
from appa_kagent_adk.plugin import AppaPluginKagent

RUNTIME_URL = "http://127.0.0.1:8787"

# The rendered config the tests' inventory comes from: one MCP server
# named by its host, and two remote agents as kagent renders them.
INVENTORY_CONFIG = {
    "model": {"type": "openai", "model": "gpt-5.2"},
    "description": "a demo agent",
    "instruction": "help with the cluster",
    "http_tools": [
        {
            "params": {"url": "http://demo-tools.kagent.svc.cluster.local:3000/mcp"},
            "tools": ["k8s_scale", "k8s_get_pods", "read_ledger", "k8s_annotate", "restart_deployment"],
        }
    ],
    "remote_agents": [
        {"name": "kagent__NS__billing_agent", "url": "http://billing-agent:8080"},
        {"name": "kagent__NS__log_analyst", "url": "http://log-analyst:8080"},
    ],
}

INVENTORY = ToolInventory.from_config(INVENTORY_CONFIG, environ={})


class FakeSession:
    def __init__(self, session_id: str = "s1", state: dict | None = None, events: list | None = None):
        self.id = session_id
        self.state = state or {}
        self.events = events or []


class FakeEvent:
    def __init__(self, content: Any = None):
        self.content = content


class FakeInvocationContext:
    def __init__(self, session: FakeSession, invocation_id: str = "i1", agent_name: str = "root-agent"):
        self.session = session
        self.invocation_id = invocation_id
        self.agent = FakeAgent(agent_name)


class FakeConfirmation:
    """ADK's ToolConfirmation on a resumed call: the person's answer."""

    def __init__(self, confirmed: bool):
        self.confirmed = confirmed


class FakeContext:
    """Stands in for CallbackContext and ToolContext alike."""

    def __init__(
        self,
        session: FakeSession,
        invocation_id: str = "i1",
        agent_name: str = "root-agent",
        tool_confirmation: FakeConfirmation | None = None,
    ):
        self.session = session
        self.invocation_id = invocation_id
        self.agent_name = agent_name
        self.tool_confirmation = tool_confirmation
        self.requested: list[tuple[str, Any]] = []

    def request_confirmation(self, hint: str = "", payload: Any = None) -> None:
        self.requested.append((hint, payload))


class FakeAgent:
    def __init__(self, name: str):
        self.name = name


class FakeTool:
    def __init__(self, name: str):
        self.name = name


class FakePart:
    def __init__(self, text: str | None = None):
        self.text = text


class FakeContent:
    def __init__(self, *texts: str):
        self.parts = [FakePart(text) for text in texts]


class Hook:
    """The scripted runtime: answers in order, records every event."""

    def __init__(self, *answers: Any):
        self.answers = list(answers)
        self.events: list[dict] = []

    def transport(self) -> httpx.MockTransport:
        def handle(request: httpx.Request) -> httpx.Response:
            self.events.append(json.loads(request.content))
            if not self.answers:
                return httpx.Response(200, json={"protocol": 1, "decision": "ack"})
            answer = self.answers.pop(0)
            if isinstance(answer, Exception):
                raise answer
            if isinstance(answer, int):
                return httpx.Response(answer, json={"error": "scripted"})
            return httpx.Response(200, json=answer)

        return httpx.MockTransport(handle)


class ScriptedRuntime(Protocol):
    """A scripted runtime: anything that hands out a /hook transport."""

    def transport(self) -> httpx.MockTransport: ...


class Remedy:
    """The scripted /mcp endpoint: records each plan the plugin ran."""

    def __init__(self, answer: str = "[appa] Authorized. Propose the call again"):
        self.answer = answer
        self.calls: list[dict] = []

    async def __call__(self, arguments: dict) -> str:
        self.calls.append(arguments)
        return self.answer


def plugin_over(
    hook: ScriptedRuntime, remedy: Remedy | None = None, inventory: ToolInventory = INVENTORY
) -> AppaPluginKagent:
    transport = hook.transport()
    return AppaPluginKagent(
        RUNTIME_URL,
        inventory=inventory,
        client_factory=lambda: httpx.AsyncClient(transport=transport),
        remedy_call=remedy,
    )


DOWN = httpx.ConnectError("connection refused")
