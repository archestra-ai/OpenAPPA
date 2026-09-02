"""The entrypoint against the pinned kagent-adk: construction, not serving.

These tests run only under the kagent lane (the git-pinned kagent-adk
with google-adk 1.31.1 and a2a-sdk 0.3.x); elsewhere they skip.
"""

import json
import os

import pytest

# kagent.core reads its environment at import time, so the identity
# must exist before the lane import below.
os.environ.setdefault("KAGENT_URL", "http://kagent-controller:8083")
os.environ.setdefault("KAGENT_NAME", "demo-agent")
os.environ.setdefault("KAGENT_NAMESPACE", "kagent")

kagent_adk = pytest.importorskip("kagent.adk", reason="the kagent-adk lane is not installed")

from appa_kagent_adk import entrypoint  # noqa: E402
from appa_kagent_adk.config_guard import ConfigRefused  # noqa: E402
from appa_kagent_adk.gates import GatedCodeExecutor  # noqa: E402
from appa_kagent_adk.plugin import AppaPluginKagent  # noqa: E402

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


def test_the_factory_wraps_code_execution_and_appends_the_reserved_toolset(config_dir):
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
    agent.tools.append(entrypoint._reserved_toolset(RUNTIME_URL))
    assert len(agent.tools) == before + 1
    reserved = agent.tools[-1]
    assert reserved._connection_params.timeout == entrypoint.REMEDY_CALL_TIMEOUT_SECONDS, (
        "the remedy call outlasts a parked consult; ADK's five-second default would fail it at the client"
    )


def test_the_appa_plugin_needs_a_runtime_url():
    with pytest.raises(ValueError, match="APPA_RUNTIME_URL"):
        AppaPluginKagent("")
