"""The equivalence checks against the pinned kagent-adk v0.9.12.

Four questions, each answered by observable behavior of the stock code:

1. Parity. The gated startup builds the agent ``kagent.adk.cli.static``
   builds, plus the reserved toolset, with ``AppaPluginKagent`` last.
2. Tool names. ``tool_names.v0.9.12.json`` records the built-in tools
   the stock builder attaches. A scripted model proposes each name
   through a real runner, and the gate sees it under that spelling.
3. Plugin order. No stock plugin overrides a gated callback, and the
   gate fires behind the stock plugins in a real runner.
4. Memory persist. The stock auto-save callback of a memory agent is
   wrapped. A denied persist writes nothing. An allowed persist writes
   once and reports one result.

These tests run only on the CI kagent v0.9.12 lane. On the locked lane
they skip.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import httpx
import pytest

# kagent.core reads its environment at import time, so the identity
# must exist before the lane import below.
os.environ.setdefault("KAGENT_URL", "http://kagent-controller:8083")
os.environ.setdefault("KAGENT_NAME", "demo-agent")
os.environ.setdefault("KAGENT_NAMESPACE", "kagent")

pytest.importorskip("kagent.adk", reason="the kagent-adk lane is not installed")

from agentsts.adk import ADKTokenPropagationPlugin  # noqa: E402
from conftest import RUNTIME_URL, Hook, plugin_over  # noqa: E402
from google.adk.apps import App  # noqa: E402
from google.adk.events.event import Event  # noqa: E402
from google.adk.models.base_llm import BaseLlm  # noqa: E402
from google.adk.models.llm_response import LlmResponse  # noqa: E402
from google.adk.plugins.base_plugin import BasePlugin  # noqa: E402
from google.adk.runners import InMemoryRunner  # noqa: E402
from google.adk.sessions.session import Session  # noqa: E402
from google.adk.tools.base_tool import BaseTool  # noqa: E402
from google.adk.tools.mcp_tool.mcp_toolset import McpToolset  # noqa: E402
from google.genai import types  # noqa: E402
from kagent.adk import AgentConfig, KAgentApp  # noqa: E402
from kagent.adk import cli as stock_cli  # noqa: E402
from kagent.adk._llm_passthrough_plugin import LLMPassthroughPlugin  # noqa: E402
from kagent.core import KAgentConfig  # noqa: E402

from appa_kagent_adk import entrypoint  # noqa: E402
from appa_kagent_adk.gates import MEMORY_PERSIST_TOOL, gate_memory_persist  # noqa: E402
from appa_kagent_adk.plugin import AppaPluginKagent  # noqa: E402
from appa_kagent_adk.wire import RESERVED_TOOL  # noqa: E402

ACK = {"protocol": 1, "decision": "ack"}
ALLOW = {"protocol": 1, "decision": "allow_call"}

CONFIG = {
    "model": {"type": "openai", "model": "gpt-5.2"},
    "description": "a demo agent",
    "instruction": "help with the cluster",
}

# An empty MemoryConfig is a valid rendered config on v0.9.12.
MEMORY_CONFIG = {**CONFIG, "memory": {}}

CARD = {
    "name": "demo-agent",
    "description": "a demo agent",
    "url": "http://demo-agent:8080",
    "version": "1.0.0",
    "capabilities": {},
    "defaultInputModes": ["text"],
    "defaultOutputModes": ["text"],
    "skills": [],
}

RECORDED_TOOLS = json.loads((Path(__file__).parent / "tool_names.v0.9.12.json").read_text())

# The gated callbacks: the ones that carry an event to the runtime. A
# stock plugin that overrides one and returns a value would end the
# plugin chain before AppaPluginKagent sees the callback.
GATED_CALLBACKS = (
    "on_user_message_callback",
    "before_tool_callback",
    "after_tool_callback",
    "on_tool_error_callback",
    "before_agent_callback",
)


# -- fixtures and helpers -----------------------------------------------


@pytest.fixture()
def config_dir(tmp_path):
    def write(config: dict) -> str:
        (tmp_path / "config.json").write_text(json.dumps(config))
        (tmp_path / "agent-card.json").write_text(json.dumps(CARD))
        return str(tmp_path)

    return write


@pytest.fixture()
def built_apps(monkeypatch) -> list[KAgentApp]:
    """Every KAgentApp a startup builds, in order.

    KAgentApp keeps the root agent factory and the plugin list as
    attributes, so a recording subclass exposes both without serving.
    Both startups import the class from ``kagent.adk``.
    """
    import kagent.adk

    built: list[KAgentApp] = []

    class RecordingApp(KAgentApp):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            built.append(self)

    monkeypatch.setattr(kagent.adk, "KAgentApp", RecordingApp)
    monkeypatch.setattr(stock_cli, "KAgentApp", RecordingApp)
    # The image env fills an unset reasoning effort. The stock startup
    # never does, so the model dumps agree only with the env scrubbed.
    monkeypatch.delenv(entrypoint.REASONING_EFFORT_ENV, raising=False)
    return built


class NoServe:
    """Stands in for uvicorn inside the stock cli: built, never served."""

    def __init__(self):
        self.served: list[Any] = []

    def run(self, server: Any, **kwargs: Any) -> None:
        self.served.append(server)


def build_stock(filepath: str, built_apps: list[KAgentApp], monkeypatch) -> tuple[Any, list]:
    """Run the stock ``kagent.adk.cli.static`` up to its serve call."""
    no_serve = NoServe()
    monkeypatch.setattr(stock_cli, "uvicorn", no_serve)
    stock_cli.static(filepath=filepath)
    assert len(no_serve.served) == 1, "the stock startup builds one server and serves it"
    app = built_apps[-1]
    return app.root_agent_factory(), app.plugins


def build_gated(filepath: str, built_apps: list[KAgentApp]) -> tuple[Any, list]:
    """Run the gated startup the way ``main`` does, without serving."""
    entrypoint.build_server(filepath, RUNTIME_URL)
    app = built_apps[-1]
    return app.root_agent_factory(), app.plugins


def stock_agent(config: dict) -> Any:
    """The stock builder's agent for a rendered config: a FunctionTool-only agent."""
    return AgentConfig.model_validate(config).to_agent(KAgentConfig().name, None, False)


def tool_names(agent: Any) -> list[str]:
    """The tool names on an agent. A toolset carries no name and drops out."""
    return [tool.name for tool in agent.tools if isinstance(tool, BaseTool)]


def recorded_tools(agent: Any) -> list[dict]:
    """The projection ``tool_names.v0.9.12.json`` records.

    ``declared`` says whether the tool hands the model a function
    declaration. Only a declared tool is dispatchable by name.
    """
    return [
        {"name": tool.name, "declared": tool._get_declaration() is not None}
        for tool in agent.tools
        if isinstance(tool, BaseTool)
    ]


def callback_names(agent: Any) -> list[str]:
    return [callback.__name__ for callback in agent.after_agent_callback or []]


class ScriptedModel(BaseLlm):
    """A model that plays a fixed list of tool calls, then answers ``done``."""

    model: str = "scripted"
    turns: list = []
    _cursor: int = 0

    async def generate_content_async(self, llm_request, stream: bool = False):
        index = self._cursor
        self._cursor += 1
        turn = self.turns[index] if index < len(self.turns) else {"text": "done"}
        if "text" in turn:
            part = types.Part(text=turn["text"])
        else:
            part = types.Part(function_call=types.FunctionCall(name=turn["tool"], args=turn.get("args", {})))
        yield LlmResponse(content=types.Content(role="model", parts=[part]))


# The answer per event kind for a whole runner turn. An event kind
# outside this set raises inside the transport, so a new event the
# plugin starts to post fails the test instead of passing by default.
ANSWERS_BY_KIND = {
    "ping": {},
    "session_start": ACK,
    "child_start": ACK,
    "prompt": ACK,
    "tool_call": ALLOW,
    "tool_result": ACK,
    "spawn_result": ACK,
    "turn_end": ACK,
}


class RunnerHook:
    """The scripted runtime for a whole runner turn.

    A runner posts liveness pings between the gated events, so this
    transport answers by event kind and records every event in order.
    """

    def __init__(self):
        self.events: list[dict] = []

    def transport(self) -> httpx.MockTransport:
        def handle(request: httpx.Request) -> httpx.Response:
            event = json.loads(request.content)
            self.events.append(event)
            return httpx.Response(200, json=ANSWERS_BY_KIND[event["event"]])

        return httpx.MockTransport(handle)

    def tool_events(self) -> list[dict]:
        return [event for event in self.events if event["event"] in ("tool_call", "tool_result")]


async def run_turn(agent: Any, plugins: list, prompt: str = "check the checkout rollout") -> None:
    """One user turn through a real runner, built the way KAgentApp builds one."""
    app_name = KAgentConfig().app_name
    runner = InMemoryRunner(app=App(name=app_name, root_agent=agent, plugins=plugins))
    session = await runner.session_service.create_session(app_name=app_name, user_id="op")
    message = types.Content(role="user", parts=[types.Part(text=prompt)])
    try:
        async for _ in runner.run_async(user_id="op", session_id=session.id, new_message=message):
            pass
    finally:
        await runner.close()


# -- 1. parity with the stock startup -----------------------------------


@pytest.mark.parametrize(
    ("propagate_token", "api_key_passthrough", "stock_plugin_types"),
    [
        (False, False, []),
        (True, False, [ADKTokenPropagationPlugin]),
        (False, True, [LLMPassthroughPlugin]),
        (True, True, [ADKTokenPropagationPlugin, LLMPassthroughPlugin]),
    ],
)
def test_the_gated_startup_builds_the_stock_agent_and_appends_the_plugin_last(
    config_dir, built_apps, monkeypatch, propagate_token, api_key_passthrough, stock_plugin_types
):
    # The stock cli reads KAGENT_PROPAGATE_TOKEN into a module global at import.
    monkeypatch.setattr(stock_cli, "propagate_token", propagate_token)
    config = {**CONFIG, "model": {**CONFIG["model"], "api_key_passthrough": api_key_passthrough}}
    filepath = config_dir(config)

    stock, stock_plugins = build_stock(filepath, built_apps, monkeypatch)
    gated, gated_plugins = build_gated(filepath, built_apps)

    assert [type(plugin) for plugin in stock_plugins] == stock_plugin_types
    assert [type(plugin) for plugin in gated_plugins] == stock_plugin_types + [AppaPluginKagent]

    assert tool_names(gated) == tool_names(stock)
    assert len(gated.tools) == len(stock.tools) + 1
    reserved = gated.tools[-1]
    assert isinstance(reserved, McpToolset)
    assert reserved.tool_filter == [RESERVED_TOOL]

    assert gated.name == stock.name
    assert gated.description == stock.description
    assert gated.static_instruction == stock.static_instruction
    assert gated.instruction == stock.instruction
    assert gated.model.model_dump() == stock.model.model_dump()
    assert gated.code_executor is None and stock.code_executor is None
    assert callback_names(gated) == callback_names(stock) == []


def test_a_memory_agent_keeps_the_stock_tools_and_swaps_only_the_persist_callback(config_dir, built_apps, monkeypatch):
    filepath = config_dir(MEMORY_CONFIG)
    stock, _ = build_stock(filepath, built_apps, monkeypatch)
    gated, _ = build_gated(filepath, built_apps)

    assert tool_names(gated) == tool_names(stock)
    assert gated.static_instruction == stock.static_instruction
    assert callback_names(stock) == ["auto_save_session_to_memory_callback"]
    assert callback_names(gated) == ["appa_gated_memory_persist"]


# -- 2. the recorded tool names -----------------------------------------


def test_the_recorded_tool_names_are_what_the_stock_builder_attaches():
    regenerated = {
        "without_memory": recorded_tools(stock_agent(CONFIG)),
        "with_memory": recorded_tools(stock_agent(MEMORY_CONFIG)),
    }
    assert regenerated == RECORDED_TOOLS, (
        "kagent-adk changed its built-in tools: re-record tool_names.v0.9.12.json and revisit the policy examples"
    )


# The arguments a scripted proposal of each recorded name carries.
PROPOSALS = {
    "ask_user": {"questions": [{"question": "which namespace?"}]},
    "prefetch_memory": {},
    "load_memory": {"query": "the checkout rollout"},
    "save_memory": {"content": "the checkout rollout is green"},
}


@pytest.mark.parametrize("recorded", RECORDED_TOOLS["with_memory"], ids=lambda recorded: recorded["name"])
async def test_a_proposal_of_each_recorded_name_crosses_the_gate_as_spelled(recorded):
    name = recorded["name"]
    agent = stock_agent(MEMORY_CONFIG)
    agent.model = ScriptedModel(turns=[{"tool": name, "args": PROPOSALS[name]}])
    hook = RunnerHook()

    if recorded["declared"]:
        await run_turn(agent, [plugin_over(hook)])
        call, result = hook.tool_events()
        assert (call["event"], call["tool"], call["arguments"]) == ("tool_call", f"builtin:{name}", PROPOSALS[name])
        assert "spawn" not in call
        assert (result["event"], result["tool"], result["outcome"]["status"]) == (
            "tool_result",
            f"builtin:{name}",
            "success",
        )
        return

    # An undeclared tool hands the model no function to call. ADK's
    # dispatcher rejects the name before any tool gate, and the
    # rejection crosses as the failure result under that spelling.
    with pytest.raises(ValueError, match=f"Tool '{name}' not found"):
        await run_turn(agent, [plugin_over(hook)])
    (failure,) = hook.tool_events()
    assert (failure["event"], failure["tool"], failure["outcome"]["status"]) == (
        "tool_result",
        f"builtin:{name}",
        "failure",
    )


# -- 3. plugin order ----------------------------------------------------


def test_no_stock_plugin_overrides_a_gated_callback(monkeypatch):
    monkeypatch.setattr(stock_cli, "propagate_token", True)
    sts = stock_cli.create_sts_integration()
    assert isinstance(sts, ADKTokenPropagationPlugin)
    for plugin in (sts, LLMPassthroughPlugin()):
        for callback in GATED_CALLBACKS:
            assert getattr(type(plugin), callback) is getattr(BasePlugin, callback), (
                f"{type(plugin).__name__} overrides {callback}: the chain would end before the gate"
            )
    for callback in GATED_CALLBACKS:
        assert getattr(AppaPluginKagent, callback) is not getattr(BasePlugin, callback)


async def test_the_gate_fires_behind_the_stock_plugins_in_a_real_runner(config_dir, built_apps, monkeypatch):
    monkeypatch.setattr(stock_cli, "propagate_token", True)
    config = {**CONFIG, "model": {**CONFIG["model"], "api_key_passthrough": True}}
    stock, stock_plugins = build_stock(config_dir(config), built_apps, monkeypatch)
    assert [type(plugin) for plugin in stock_plugins] == [ADKTokenPropagationPlugin, LLMPassthroughPlugin]

    stock.model = ScriptedModel(turns=[{"tool": "ask_user", "args": PROPOSALS["ask_user"]}])
    hook = RunnerHook()
    await run_turn(stock, [*stock_plugins, plugin_over(hook)])

    kinds = [event["event"] for event in hook.events]
    assert kinds[:2] == ["session_start", "prompt"]
    assert [event["tool"] for event in hook.tool_events()] == ["ask_user", "ask_user"]
    assert kinds[-1] == "turn_end"


# -- 4. the memory persist of a real memory agent -------------------------


class RecordingMemoryService:
    """Stands in for KagentMemoryService, whose persist runs an HTTP background task."""

    def __init__(self):
        self.calls: list[tuple[Any, Any]] = []

    async def add_session_to_memory(self, session: Any, model: Any = None) -> None:
        self.calls.append((session, model))


class PersistContext:
    """The CallbackContext shape the stock persist callback reads."""

    def __init__(self, session: Session, memory_service: RecordingMemoryService):
        self._invocation_context = SimpleNamespace(session=session, memory_service=memory_service)


def session_after_user_turns(count: int) -> Session:
    """A session whose event log holds ``count`` user turns.

    The stock callback persists on every fifth user turn and skips the
    others, so an allowed persist is observable only at that cadence.
    """
    events = [Event(author="user", invocation_id=f"i{n}") for n in range(count)]
    return Session(id="s1", app_name=KAgentConfig().app_name, user_id="op", events=events)


async def test_a_denied_persist_of_a_real_memory_agent_writes_nothing():
    agent = stock_agent(MEMORY_CONFIG)
    hook = Hook({"protocol": 1, "decision": "deny_call", "feedback": "blocked: the session holds confidential values"})
    plugin = plugin_over(hook)
    assert gate_memory_persist(agent, plugin) is True
    assert callback_names(agent) == ["appa_gated_memory_persist"]

    service = RecordingMemoryService()
    await agent.after_agent_callback[0](PersistContext(session_after_user_turns(5), service))

    assert service.calls == [], "a denied persist never reaches the memory service"
    assert [(event["event"], event["tool"]) for event in hook.events] == [("tool_call", MEMORY_PERSIST_TOOL)]


async def test_an_allowed_persist_of_a_real_memory_agent_writes_once_and_reports_once():
    agent = stock_agent(MEMORY_CONFIG)
    hook = Hook(ALLOW, ACK)
    plugin = plugin_over(hook)
    assert gate_memory_persist(agent, plugin) is True

    service = RecordingMemoryService()
    session = session_after_user_turns(5)
    await agent.after_agent_callback[0](PersistContext(session, service))

    assert len(service.calls) == 1
    persisted, model = service.calls[0]
    assert persisted is session
    assert model is agent.model, "the stock callback hands its own model over for the summary"
    assert [(event["event"], event["tool"]) for event in hook.events] == [
        ("tool_call", MEMORY_PERSIST_TOOL),
        ("tool_result", MEMORY_PERSIST_TOOL),
    ]
    assert hook.events[1]["outcome"] == {"status": "success", "body": {"persisted": True}}
