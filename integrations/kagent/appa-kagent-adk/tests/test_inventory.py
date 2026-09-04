"""The tool inventory: what a rendered config lets the wire name, and how."""

import json
from pathlib import Path

import pytest

from appa_kagent_adk import wire
from appa_kagent_adk.config_guard import ConfigRefused
from appa_kagent_adk.gates import CODE_EXECUTION_TOOL, MEMORY_PERSIST_TOOL
from appa_kagent_adk.inventory import SKILLS_FOLDER_ENV, ToolInventory, builtin_manifest, is_spawn

KAGENT = Path(__file__).parent.parent.parent
SHARED_MANIFEST = KAGENT / "fixtures" / "kagent-builtins.json"
GO_MANIFEST = KAGENT / "appa-kagent-adk-go" / "builtins.json"
PYTHON_MANIFEST = KAGENT / "appa-kagent-adk" / "src" / "appa_kagent_adk" / "builtins.json"

BASE = {"model": {"type": "openai", "model": "gpt-5.2"}, "description": "d", "instruction": "i"}
DEMO_TOOLS = {"params": {"url": "http://demo-tools.kagent.svc.cluster.local:3000/mcp"}, "tools": ["list_pods"]}


def inventory(**fields) -> ToolInventory:
    return ToolInventory.from_config({**BASE, **fields}, environ={})


def test_each_class_spells_its_tools():
    built = inventory(
        http_tools=[DEMO_TOOLS],
        sse_tools=[{"params": {"url": "https://kagent-tool-server:8084/sse"}, "tools": ["k8s_get_resources"]}],
        remote_agents=[{"name": "kagent__NS__log_analyst", "url": "http://log-analyst:8080"}],
    )
    assert built.spelling("list_pods") == "mcp:demo-tools/list_pods"
    assert built.spelling("k8s_get_resources") == "mcp:kagent-tool-server/k8s_get_resources"
    assert built.spelling("kagent__NS__log_analyst") == "agent:kagent/log-analyst"
    assert built.spelling("ask_user") == "builtin:ask_user"
    assert built.spelling(wire.RESERVED_TOOL) == wire.CONTROL_TOOL
    assert built.spelling("k8s_delete_namespace") is None
    assert (CODE_EXECUTION_TOOL, MEMORY_PERSIST_TOOL) == ("gate:code_execution", "gate:memory_persist")


def test_only_the_agent_class_is_a_spawn():
    assert is_spawn("agent:kagent/log-analyst")
    assert not is_spawn("mcp:demo-tools/list_pods")
    assert not is_spawn("builtin:ask_user")
    assert not is_spawn(wire.CONTROL_TOOL)


def test_a_remote_agent_name_unmangles_both_labels():
    built = inventory(remote_agents=[{"name": "team_a__NS__release_manager_go", "url": "http://x"}])
    assert built.spelling("team_a__NS__release_manager_go") == "agent:team-a/release-manager-go"


@pytest.mark.parametrize(
    "server",
    [
        pytest.param({"params": {"url": "http://demo-tools:3000/mcp"}}, id="no-filter"),
        pytest.param({"params": {"url": "http://demo-tools:3000/mcp"}, "tools": []}, id="empty-filter"),
        pytest.param({"params": {"url": "http://demo-tools:3000/mcp"}, "tools": "list_pods"}, id="not-a-list"),
    ],
)
def test_an_mcp_entry_without_a_tool_filter_is_refused(server):
    with pytest.raises(ConfigRefused, match=r"http_tools\.0"):
        inventory(http_tools=[server])


@pytest.mark.parametrize(
    "server",
    [
        pytest.param({"params": {}, "tools": ["a"]}, id="no-url"),
        pytest.param({"params": {"url": "/mcp"}, "tools": ["a"]}, id="no-host"),
        pytest.param({"params": {"url": "http://demo-tools:3000/mcp"}, "tools": ["list pods"]}, id="tool-with-space"),
        pytest.param(
            {"params": {"url": "http://demo__tools:3000/mcp"}, "tools": ["a"]}, id="host-with-the-reserved-mark"
        ),
    ],
)
def test_a_name_the_wire_cannot_spell_is_refused(server):
    with pytest.raises(ConfigRefused, match=r"http_tools\.0"):
        inventory(http_tools=[server])


@pytest.mark.parametrize("name", ["log-analyst", "__NS__log_analyst", "kagent__NS__", "a__NS__b__NS__c"])
def test_a_remote_agent_outside_the_rendered_shape_is_refused(name):
    with pytest.raises(ConfigRefused, match=r"remote_agents\.0\.name"):
        inventory(remote_agents=[{"name": name, "url": "http://x"}])


def test_a_raw_name_declared_twice_is_refused():
    with pytest.raises(ConfigRefused, match="list_pods"):
        inventory(http_tools=[DEMO_TOOLS, {"params": {"url": "http://other:3000/mcp"}, "tools": ["list_pods"]}])
    with pytest.raises(ConfigRefused, match="ask_user"):
        inventory(http_tools=[{"params": {"url": "http://other:3000/mcp"}, "tools": ["ask_user"]}])


def test_two_raw_names_that_spell_alike_are_refused():
    # kagent renders a hyphen as an underscore, so these two distinct raw
    # names carry one spelling, and one of them would be lost.
    with pytest.raises(ConfigRefused, match=r"agent:team-a/release-manager"):
        inventory(
            remote_agents=[
                {"name": "team_a__NS__release_manager", "url": "http://x"},
                {"name": "team-a__NS__release_manager", "url": "http://y"},
            ]
        )


def test_every_spelling_despells_back_to_the_name_adk_dispatches():
    built = inventory(
        http_tools=[DEMO_TOOLS],
        remote_agents=[{"name": "kagent__NS__log_analyst", "url": "http://x"}],
    )
    for name, spelling in built.spellings.items():
        assert built.despell(f"call {spelling} now") == f"call {name} now"


def test_the_builtin_groups_follow_the_config_and_the_environment():
    plain = inventory()
    assert plain.spelling("load_memory") is None
    assert plain.spelling("read_file") is None

    with_memory = inventory(memory={})
    assert with_memory.spelling("load_memory") == "builtin:load_memory"
    assert with_memory.spelling("prefetch_memory") == "builtin:prefetch_memory"

    with_skills = ToolInventory.from_config(BASE, environ={SKILLS_FOLDER_ENV: "/skills"})
    assert with_skills.spelling("skills") == "builtin:skills"
    assert with_skills.spelling("read_file") == "builtin:read_file"
    assert with_skills.spelling("bash") == "builtin:bash"
    assert ToolInventory.from_config(BASE, environ={SKILLS_FOLDER_ENV: " "}).spelling("skills") is None


def test_the_packaged_manifest_is_the_shared_one_and_the_go_copy():
    shared = SHARED_MANIFEST.read_bytes()
    assert PYTHON_MANIFEST.read_bytes() == shared
    assert GO_MANIFEST.read_bytes() == shared
    assert builtin_manifest() == json.loads(shared)
    assert set(json.loads(shared)) == {"python", "go"}
