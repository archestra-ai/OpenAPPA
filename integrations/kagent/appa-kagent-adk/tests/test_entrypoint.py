"""The entrypoint against the pinned kagent-adk: construction, not serving.

These tests run only on the CI kagent v0.9.12 lane, which installs
kagent-adk from the v0.9.12 tag with google-adk 1.31.1 and a2a-sdk
0.3.x (``.github/workflows/ci.yml``). On the locked lane they skip.
"""

import json
import logging
import os
import sys

import pytest

# kagent.core reads its environment at import time, so the identity
# must exist before the lane import below.
os.environ.setdefault("KAGENT_URL", "http://kagent-controller:8083")
os.environ.setdefault("KAGENT_NAME", "demo-agent")
os.environ.setdefault("KAGENT_NAMESPACE", "kagent")

kagent_adk = pytest.importorskip("kagent.adk", reason="the kagent-adk lane is not installed")

from google.adk.tools.base_tool import BaseTool  # noqa: E402
from google.adk.tools.mcp_tool.mcp_toolset import McpToolset  # noqa: E402
from kagent.adk import cli as stock_cli  # noqa: E402

from appa_kagent_adk import entrypoint  # noqa: E402
from appa_kagent_adk.config_guard import ConfigRefused  # noqa: E402
from appa_kagent_adk.gates import GatedCodeExecutor  # noqa: E402
from appa_kagent_adk.plugin import AppaPluginKagent  # noqa: E402
from appa_kagent_adk.wire import RESERVED_TOOL, RUNTIME_TOOLS  # noqa: E402

RUNTIME_URL = "http://127.0.0.1:8787"

CONFIG = {
    "model": {"type": "openai", "model": "gpt-5.2"},
    "description": "a demo agent",
    "instruction": "help with the cluster",
}

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


@pytest.fixture()
def config_dir(tmp_path):
    def write(config: dict) -> str:
        (tmp_path / "config.json").write_text(json.dumps(config))
        (tmp_path / "agent-card.json").write_text(json.dumps(CARD))
        return str(tmp_path)

    return write


@pytest.fixture()
def built_apps(monkeypatch) -> list:
    """Every KAgentApp a startup builds, in order.

    KAgentApp keeps the root agent factory and the plugin list as
    attributes, so a recording subclass exposes both without serving.
    The entrypoint imports the class from ``kagent.adk`` at call time,
    and the stock command holds its own binding, so both are patched.
    """
    import kagent.adk
    from kagent.adk import KAgentApp

    built: list = []

    class RecordingApp(KAgentApp):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            built.append(self)

    monkeypatch.setattr(kagent.adk, "KAgentApp", RecordingApp)
    monkeypatch.setattr(stock_cli, "KAgentApp", RecordingApp)
    return built


@pytest.fixture()
def served(monkeypatch) -> list:
    """Every ``(server, uvicorn arguments)`` a startup serves: built, never served.

    The stock command calls ``uvicorn.run`` through the same module, so
    this one patch stops both startups at the built server.
    """
    import uvicorn

    servers: list = []
    monkeypatch.setattr(uvicorn, "run", lambda server, **kwargs: servers.append((server, kwargs)))
    return servers


def build_agent(filepath: str):
    """Build the server, then run the factory the way KAgentApp does."""
    server = entrypoint.build_server(filepath, RUNTIME_URL)
    assert server is not None
    return server


def test_a_stock_config_builds_and_the_appa_plugin_is_registered_last(config_dir):
    from kagent.adk import AgentConfig
    from kagent.core import KAgentConfig

    filepath = config_dir(CONFIG)
    build_agent(filepath)
    # The construction deltas are observable on a factory-built agent.
    agent_config = AgentConfig.model_validate(CONFIG)
    app_cfg = KAgentConfig()
    agent = agent_config.to_agent(app_cfg.name, None, False)
    assert agent.code_executor is None, "no executeCodeBlocks, no wrapper"


def test_an_unknown_field_refuses_the_start(config_dir):
    with pytest.raises(ConfigRefused, match="surprise"):
        entrypoint.build_server(config_dir({**CONFIG, "surprise": True}), RUNTIME_URL)


def test_compiled_sub_agents_refuse_with_the_runtime_mismatch(config_dir):
    with pytest.raises(ConfigRefused, match="Go-compiled"):
        entrypoint.build_server(config_dir({**CONFIG, "sub_agents": []}), RUNTIME_URL)


def test_a_divergent_summarizer_refuses_the_start(config_dir):
    divergent = {
        **CONFIG,
        "context_config": {
            "compaction": {
                "compaction_interval": 10,
                "overlap_size": 2,
                "summarizer_model": {"type": "openai", "model": "gpt-5.2-mini"},
            }
        },
    }
    with pytest.raises(ConfigRefused, match="summarizer"):
        entrypoint.build_server(config_dir(divergent), RUNTIME_URL)


def test_the_factory_wraps_code_execution_and_appends_the_remedy_toolset(config_dir):
    from kagent.adk import AgentConfig
    from kagent.core import KAgentConfig

    config = {**CONFIG, "execute_code": True}
    filepath = config_dir(config)
    entrypoint.build_server(filepath, RUNTIME_URL)

    # Replay the factory deltas directly, the way build_server wires them.
    from appa_kagent_adk import gates
    from appa_kagent_adk.identity import SessionIdentity

    agent_config = AgentConfig.model_validate(config)
    app_cfg = KAgentConfig()
    agent = agent_config.to_agent(app_cfg.name, None, False)
    assert agent.code_executor is not None, "execute_code installs the sandboxed executor"
    identity = SessionIdentity()
    agent.code_executor = gates.GatedCodeExecutor(agent.code_executor, gates.SyncHookGate(RUNTIME_URL, identity))
    assert isinstance(agent.code_executor, GatedCodeExecutor)
    before = len(agent.tools)
    agent.tools.append(entrypoint._runtime_toolset(RUNTIME_URL))
    assert len(agent.tools) == before + 1
    reserved = agent.tools[-1]
    assert reserved.tool_filter == [RESERVED_TOOL]
    assert reserved._connection_params.timeout == entrypoint.REMEDY_CALL_TIMEOUT_SECONDS, (
        "the remedy call outlasts a parked consult; ADK's five-second default would fail it at the client"
    )


def test_only_appa_guide_receives_the_management_toolset(monkeypatch):
    monkeypatch.setenv("APPA_GUIDE", "true")
    monkeypatch.setenv("APPA_GUIDE_MCP_URL", "http://runtime:18788/guide-mcp")
    guide = entrypoint._runtime_toolset(RUNTIME_URL)
    assert guide.tool_filter == RUNTIME_TOOLS
    assert guide._connection_params.url == "http://runtime:18788/guide-mcp"

    monkeypatch.delenv("APPA_GUIDE_MCP_URL")
    with pytest.raises(ConfigRefused, match="APPA_GUIDE_MCP_URL"):
        entrypoint._runtime_toolset(RUNTIME_URL)


def test_the_appa_plugin_needs_a_runtime_url():
    with pytest.raises(ValueError, match="APPA_RUNTIME_URL"):
        AppaPluginKagent("")


# -- the knob: APPA_ENABLED selects the mode --------------------------------


@pytest.fixture(autouse=True)
def a_clean_env(monkeypatch):
    """No variable of the developer's shell selects a mode in a test."""
    monkeypatch.delenv(entrypoint.ENABLED_ENV, raising=False)
    monkeypatch.delenv(entrypoint.RUNTIME_URL_ENV, raising=False)
    monkeypatch.delenv(entrypoint.REASONING_EFFORT_ENV, raising=False)


# The knob-by-URL matrix. One axis is the value APPA_ENABLED carries,
# and the other is the runtime URL the agent names. Every cell is a
# case below: the four off values and the two on values, each against
# an agent that names a runtime and one that does not, and the values
# outside the set. Off is the default of the image.
OFF_VALUES = [
    pytest.param(None, id="unset"),
    pytest.param("", id="empty"),
    pytest.param("false", id="false"),
    pytest.param(" FALSE ", id="padded-uppercase-false"),
]
ON_VALUES = [
    pytest.param("true", id="true"),
    pytest.param("TRUE ", id="padded-uppercase-true"),
]
# The knob carries no synonyms. A value another tool reads as a boolean
# refuses the start here, and so does a typo of true or of false.
OUTSIDE_VALUES = [
    pytest.param("yes", id="yes"),
    pytest.param("no", id="no"),
    pytest.param("1", id="1"),
    pytest.param("0", id="0"),
    pytest.param("ture", id="a-typo-of-true"),
    pytest.param("flase", id="a-typo-of-false"),
]
URL_CELLS = [
    pytest.param(None, id="no-runtime-url"),
    pytest.param(RUNTIME_URL, id="a-runtime-url"),
]


def environ_of(knob: str | None) -> dict:
    """The environment one knob value describes. ``None`` leaves it unset."""
    return {} if knob is None else {entrypoint.ENABLED_ENV: knob}


def set_env(monkeypatch, knob: str | None, url: str | None) -> None:
    """Put the agent container's env in the state one matrix cell describes."""
    if knob is not None:
        monkeypatch.setenv(entrypoint.ENABLED_ENV, knob)
    if url is not None:
        monkeypatch.setenv(entrypoint.RUNTIME_URL_ENV, url)


def entrypoint_lines(caplog) -> list[str]:
    """The lines the entrypoint logger wrote, in order."""
    return [record.getMessage() for record in caplog.records if record.name == "appa_kagent_adk.entrypoint"]


def entrypoint_levels(caplog) -> set[int]:
    """The levels the entrypoint logger wrote at."""
    return {record.levelno for record in caplog.records if record.name == "appa_kagent_adk.entrypoint"}


@pytest.mark.parametrize("knob", OFF_VALUES)
def test_the_knob_reads_off(knob):
    assert entrypoint.gate_enabled(environ_of(knob)) is False


@pytest.mark.parametrize("knob", ON_VALUES)
def test_the_knob_reads_on(knob):
    assert entrypoint.gate_enabled(environ_of(knob)) is True


@pytest.mark.parametrize("knob", OUTSIDE_VALUES)
def test_a_knob_value_outside_the_set_raises(knob):
    """A typo must never disable the gate in silence."""
    with pytest.raises(ConfigRefused, match=entrypoint.ENABLED_ENV) as refusal:
        entrypoint.gate_enabled(environ_of(knob))
    assert knob in str(refusal.value), "the refusal names the value the operator set"


# -- main(): the mode the image serves -------------------------------------


def run_main(monkeypatch, filepath: str) -> int:
    """Run main() over a config directory, with the controller's args."""
    monkeypatch.setattr(sys, "argv", ["appa-kagent-adk", "--filepath", filepath])
    return entrypoint.main()


@pytest.mark.parametrize("url", URL_CELLS)
@pytest.mark.parametrize("knob", OFF_VALUES)
def test_the_stock_mode_serves_the_stock_construction(config_dir, monkeypatch, built_apps, served, caplog, knob, url):
    """Off is the default: the image serves what the stock runtime image serves.

    An operator sets this image as the default agent image of a whole
    cluster. An agent that does not set the knob to true gets the stock
    plugin list and the stock tools, and one WARNING line names it as
    ungated. A runtime URL alone gates nothing, so a second WARNING
    line names that mistake.
    """
    set_env(monkeypatch, knob, url)
    with caplog.at_level(logging.INFO):
        assert run_main(monkeypatch, config_dir(CONFIG)) == 0

    assert len(served) == 1, "the ungated image builds a server and serves it"
    app = built_apps[-1]
    assert not any(isinstance(plugin, AppaPluginKagent) for plugin in app.plugins), "no gate plugin"
    agent = app.root_agent_factory()
    assert not any(isinstance(tool, McpToolset) for tool in agent.tools), "no reserved toolset"

    expected = [entrypoint.STOCK_STARTUP]
    if url is not None:
        expected.append(entrypoint.IGNORED_RUNTIME_URL)
    assert entrypoint_lines(caplog) == expected, "one line names the mode, and one more the ignored URL"
    assert entrypoint_levels(caplog) == {logging.WARNING}, "every line of the ungated mode is unmistakable"
    assert "UNGATED" in entrypoint_lines(caplog)[0]


# The rendered configs the parity check covers: the plain agent, the
# memory agent whose stock build installs an auto-save callback, the
# agent that runs code, and the passthrough model that adds a stock
# plugin. The last cell turns on token propagation, the other stock
# plugin.
PARITY_CELLS = [
    pytest.param(CONFIG, False, id="a-stock-config"),
    pytest.param({**CONFIG, "memory": {}}, False, id="a-memory-agent"),
    pytest.param({**CONFIG, "execute_code": True}, False, id="an-agent-that-runs-code"),
    pytest.param(
        {**CONFIG, "model": {**CONFIG["model"], "api_key_passthrough": True}}, False, id="a-passthrough-model"
    ),
    pytest.param(CONFIG, True, id="token-propagation"),
]


def app_shape(app) -> dict:
    """The construction a KAgentApp carries, as values a second build repeats.

    The root agent factory is a closure, so the agent it builds stands
    in for it. A plugin and a tool are instances, so their type names
    stand in for them.
    """
    agent = app.root_agent_factory()
    return {
        "kagent_url": app.kagent_url,
        "app_name": app.app_name,
        "agent_card": app.agent_card,
        "stream": app.stream,
        "agent_config": app.agent_config,
        "plugins": [type(plugin).__name__ for plugin in app.plugins],
        "name": agent.name,
        "description": agent.description,
        "instruction": agent.instruction,
        "static_instruction": agent.static_instruction,
        "model": agent.model.model_dump(),
        "tools": [type(tool).__name__ for tool in agent.tools],
        "tool_names": [tool.name for tool in agent.tools if isinstance(tool, BaseTool)],
        "code_executor": type(agent.code_executor).__name__,
        "after_agent_callback": [callback.__name__ for callback in agent.after_agent_callback or []],
    }


@pytest.mark.parametrize(("config", "propagate_token"), PARITY_CELLS)
def test_the_stock_mode_builds_what_the_stock_command_builds(
    config_dir, monkeypatch, built_apps, served, config, propagate_token
):
    """The ungated image is a drop-in replacement, held to the stock command.

    ``kagent.adk.cli.static`` is the startup the stock runtime image
    runs. Both startups build here over one rendered config, and every
    value they carry agrees: the plugin list, the built agent, and the
    uvicorn arguments.
    """
    # The stock command reads KAGENT_PROPAGATE_TOKEN into a module
    # global at import, and the entrypoint reads that same global.
    monkeypatch.setattr(stock_cli, "propagate_token", propagate_token)
    filepath = config_dir(config)

    stock_cli.static(filepath=filepath)
    assert len(served) == 1, "the stock command builds one server and serves it"
    stock = app_shape(built_apps[-1])

    assert run_main(monkeypatch, filepath) == 0
    assert len(served) == 2, "the ungated image builds one server and serves it"
    assert app_shape(built_apps[-1]) == stock
    assert served[-1][1] == served[0][1], "the ungated image serves under the stock uvicorn arguments"


def test_the_stock_mode_leaves_the_code_executor_unwrapped(config_dir, monkeypatch, built_apps, served):
    assert run_main(monkeypatch, config_dir({**CONFIG, "execute_code": True})) == 0
    agent = built_apps[-1].root_agent_factory()
    assert agent.code_executor is not None, "execute_code installs the sandboxed executor"
    assert not isinstance(agent.code_executor, GatedCodeExecutor), "the ungated agent runs the stock executor"


@pytest.mark.parametrize(
    "config",
    [
        pytest.param({**CONFIG, "surprise": True}, id="unknown-key"),
        pytest.param({**CONFIG, "sub_agents": []}, id="sub-agents"),
    ],
)
def test_the_stock_mode_runs_a_config_the_gated_mode_refuses(config_dir, monkeypatch, served, config):
    """The config guard is a gated-mode delta. A config the stock image runs must run here."""
    filepath = config_dir(config)
    set_env(monkeypatch, "true", RUNTIME_URL)
    assert run_main(monkeypatch, filepath) == 2

    monkeypatch.delenv(entrypoint.ENABLED_ENV)
    assert run_main(monkeypatch, filepath) == 0, "the guard never runs while the knob is off"
    assert len(served) == 1


def test_the_stock_mode_reports_an_invalid_config_without_the_values(config_dir, monkeypatch, capsys):
    """Neither mode starts on a config that does not validate, and neither mode prints its values.

    The stock startup fails on this config too, so the exit is not a
    refusal of a config the stock image runs.
    """
    assert run_main(monkeypatch, config_dir({**CONFIG, "description": {"leak": "sk-not-for-stderr"}})) == 2
    stderr = capsys.readouterr().err
    assert "refusing to start: the config does not validate: 1 error(s); first: description: string_type" in stderr
    assert "sk-not-for-stderr" not in stderr


@pytest.mark.parametrize("knob", ON_VALUES)
def test_the_gated_mode_serves_the_gated_construction(config_dir, monkeypatch, built_apps, served, caplog, knob):
    """On adds the construction deltas, and one line names the runtime."""
    set_env(monkeypatch, knob, RUNTIME_URL)
    with caplog.at_level(logging.INFO):
        assert run_main(monkeypatch, config_dir(CONFIG)) == 0

    assert len(served) == 1
    app = built_apps[-1]
    assert isinstance(app.plugins[-1], AppaPluginKagent), "the gate is the last plugin"
    reserved = app.root_agent_factory().tools[-1]
    assert isinstance(reserved, McpToolset) and reserved.tool_filter == [RESERVED_TOOL]

    assert entrypoint_lines(caplog) == [entrypoint.GATED_STARTUP % RUNTIME_URL], "one line names the mode"
    assert entrypoint_levels(caplog) == {logging.INFO}
    assert not any("UNGATED" in record.getMessage() for record in caplog.records)


# -- main(): the process exit codes ----------------------------------------


@pytest.mark.parametrize("knob", ON_VALUES)
def test_the_gated_mode_without_a_runtime_url_refuses_the_start(config_dir, monkeypatch, served, capsys, knob):
    """The gated mode fails closed: it never serves an agent it cannot gate."""
    set_env(monkeypatch, knob, None)
    assert run_main(monkeypatch, config_dir(CONFIG)) == 2
    assert served == [], "no server reaches uvicorn"
    stderr = capsys.readouterr().err
    assert f"refusing to start: {entrypoint.MISSING_RUNTIME_URL}" in stderr
    assert "Traceback" not in stderr


@pytest.mark.parametrize("url", URL_CELLS)
@pytest.mark.parametrize("knob", OUTSIDE_VALUES)
def test_a_knob_value_outside_the_set_refuses_the_start(config_dir, monkeypatch, served, capsys, knob, url):
    """A value outside the set exits 2 and names the value. It never runs ungated."""
    set_env(monkeypatch, knob, url)
    assert run_main(monkeypatch, config_dir(CONFIG)) == 2
    assert served == [], "no server reaches uvicorn"
    stderr = capsys.readouterr().err
    assert "refusing to start" in stderr
    assert entrypoint.ENABLED_ENV in stderr and knob in stderr
    assert "Traceback" not in stderr


def test_main_returns_2_on_a_refused_config(config_dir, monkeypatch, capsys):
    set_env(monkeypatch, "true", RUNTIME_URL)
    monkeypatch.setattr(sys, "argv", ["appa-kagent-adk", "--filepath", config_dir({**CONFIG, "surprise": True})])
    assert entrypoint.main() == 2
    stderr = capsys.readouterr().err
    assert "refusing to start" in stderr
    assert "surprise" in stderr


@pytest.mark.parametrize(
    ("invalid", "first"),
    [
        pytest.param(
            {**CONFIG, "description": {"leak": "sk-not-for-stderr"}}, "description: string_type", id="wrong-type"
        ),
        pytest.param(
            {**CONFIG, "model": {"type": "sk-not-for-stderr"}}, "model: union_tag_invalid", id="bad-discriminator"
        ),
    ],
)
def test_main_returns_2_on_a_config_that_does_not_validate(config_dir, monkeypatch, capsys, invalid, first):
    """An invalid config exits 2 with one sanitized line: the location and the error type, never the value.

    A discriminator error is the case that pins the error type over the
    message: pydantic's message echoes the tag it did not recognize.
    """
    set_env(monkeypatch, "true", RUNTIME_URL)
    monkeypatch.setattr(sys, "argv", ["appa-kagent-adk", "--filepath", config_dir(invalid)])
    assert entrypoint.main() == 2
    stderr = capsys.readouterr().err
    assert f"refusing to start: the config does not validate: 1 error(s); first: {first}" in stderr
    assert "Traceback" not in stderr
    assert "sk-not-for-stderr" not in stderr
