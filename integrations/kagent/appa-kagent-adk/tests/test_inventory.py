"""The tool inventory: what a rendered config lets the wire name, and how."""

import json
import re
import time
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
        # A boundary period ends the spelling run short, so despell
        # could never match the spelling of such a name back.
        pytest.param({"params": {"url": "http://demo-tools:3000/mcp"}, "tools": [".status"]}, id="a-leading-period"),
        pytest.param({"params": {"url": "http://demo-tools:3000/mcp"}, "tools": ["status."]}, id="a-trailing-period"),
        pytest.param({"params": {"url": "http://demo-tools:3000/mcp"}, "tools": ["."]}, id="a-lone-period"),
    ],
)
def test_a_name_the_wire_cannot_spell_is_refused(server):
    with pytest.raises(ConfigRefused, match=r"http_tools\.0"):
        inventory(http_tools=[server])


@pytest.mark.parametrize(
    "host",
    [
        pytest.param("demo-tools", id="the-service"),
        pytest.param("demo-tools.kagent", id="the-service-and-namespace"),
        pytest.param("demo-tools.kagent.svc", id="the-svc-form"),
        pytest.param("demo-tools.kagent.svc.cluster.local", id="the-fully-qualified-form"),
        pytest.param("demo-tools.kagent.svc.cluster.local.", id="the-absolute-form"),
        pytest.param("localhost", id="loopback-by-name"),
        pytest.param("127.0.0.1", id="loopback-by-address"),
    ],
)
def test_an_in_cluster_endpoint_names_its_toolset(host):
    built = inventory(http_tools=[{"params": {"url": f"http://{host}:3000/mcp"}, "tools": ["list_pods"]}])
    assert built.spelling("list_pods") == f"mcp:{host.split('.')[0]}/list_pods"


def test_a_host_written_in_another_case_is_the_same_toolset():
    """DNS is case-insensitive, so the same service reaches the same policy
    identity however the URL spells it."""
    url = "http://DEMO-TOOLS.kagent.svc.cluster.local:3000/mcp"
    built = inventory(http_tools=[{"params": {"url": url}, "tools": ["list_pods"]}])
    assert built.spelling("list_pods") == "mcp:demo-tools/list_pods"


@pytest.mark.parametrize(
    "host",
    [
        # The attack: a toolset name a trusted policy already names,
        # served by an authority the cluster does not resolve.
        pytest.param("demo-tools.attacker.example.com", id="a-foreign-domain"),
        pytest.param("demo-tools.kagent.example.com", id="a-foreign-domain-under-the-namespace"),
        pytest.param("demo-tools.kagent.svc.attacker.com", id="a-foreign-domain-past-svc"),
        pytest.param("demo-tools.kagent.pod.cluster.local", id="a-form-that-is-not-a-service"),
        pytest.param("demo-tools.kagent.svc.cluster.local.attacker.com", id="a-suffix-past-the-cluster-domain"),
        pytest.param("192.0.2.10", id="an-address-outside-loopback"),
    ],
)
def test_an_mcp_endpoint_outside_the_cluster_is_refused(host):
    """The toolset is the host's first label, so a foreign endpoint under that
    label would take the policy identity of the in-cluster service."""
    server = {"params": {"url": f"http://{host}:3000/mcp"}, "tools": ["list_pods"]}
    with pytest.raises(ConfigRefused, match=re.escape(host)):
        inventory(http_tools=[server])


@pytest.mark.parametrize("name", ["log-analyst", "__NS__log_analyst", "kagent__NS__", "a__NS__b__NS__c"])
def test_a_remote_agent_outside_the_rendered_shape_is_refused(name):
    with pytest.raises(ConfigRefused, match=r"remote_agents\.0\.name"):
        inventory(remote_agents=[{"name": name, "url": "http://x"}])


@pytest.mark.parametrize("remote", [{"url": "http://x"}, {"name": None, "url": "http://x"}])
def test_a_nameless_remote_agent_is_refused(remote):
    """The runtime wires a remote agent as a tool of its name. An entry that
    declares none leaves a wired tool the inventory cannot spell."""
    with pytest.raises(ConfigRefused, match=r"remote_agents\.0\.name"):
        inventory(remote_agents=[remote])


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


def test_despelling_a_long_result_costs_time_in_its_length():
    """A tool result is text the model produced, and one long identifier in it
    must not cost the agent its event loop: the scan is anchored on the class a
    spelling starts with, so a run that carries none is walked once. Unanchored
    it is quadratic — 16 KB took a second, and this input would take minutes."""
    built = inventory(http_tools=[DEMO_TOOLS])
    text = "x" * 400_000
    started = time.monotonic()
    assert built.despell(text) == text
    assert time.monotonic() - started < 2.0


def test_the_inventory_carries_one_bijection_no_caller_can_contradict():
    """One forward mapping is the whole state: the inverse is derived from it,
    both directions are read-only, and a forward mapping with no inverse is
    refused rather than silently losing one of the two raw names."""
    built = inventory(
        http_tools=[DEMO_TOOLS],
        remote_agents=[{"name": "kagent__NS__log_analyst", "url": "http://x"}],
    )
    assert built.names == {spelling: name for name, spelling in built.spellings.items()}
    with pytest.raises(TypeError):
        built.spellings["list_pods"] = "mcp:other/list_pods"
    with pytest.raises(TypeError):
        built.names["mcp:demo-tools/list_pods"] = "other"
    with pytest.raises(ValueError):
        ToolInventory({"list_pods": "mcp:demo-tools/list_pods", "list_pods_v2": "mcp:demo-tools/list_pods"})


def test_every_spelling_despells_back_to_the_name_adk_dispatches():
    built = inventory(
        http_tools=[DEMO_TOOLS],
        remote_agents=[{"name": "kagent__NS__log_analyst", "url": "http://x"}],
    )
    for name, spelling in built.spellings.items():
        assert built.despell(f"call {spelling} now") == f"call {name} now"


LIST_PODS = "mcp:demo-tools/list_pods"
LOG_ANALYST = "agent:kagent/log-analyst"


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        # The plain cases: a spelling the runtime named, whole.
        pytest.param(f"call {LIST_PODS} now", "call list_pods now", id="in-a-sentence"),
        pytest.param(LIST_PODS, "list_pods", id="the-whole-text"),
        pytest.param(f"blocked {LOG_ANALYST}", "blocked kagent__NS__log_analyst", id="the-agent-class"),
        pytest.param(
            f"{LIST_PODS} then {LOG_ANALYST}.",
            "list_pods then kagent__NS__log_analyst.",
            id="two-spellings",
        ),
        # Punctuation after a spelling is punctuation, and it stands.
        pytest.param(f"Retry {LIST_PODS}.", "Retry list_pods.", id="a-period"),
        pytest.param(f"Retry {LIST_PODS}, then stop", "Retry list_pods, then stop", id="a-comma"),
        pytest.param(f"blocked {LIST_PODS}: no body", "blocked list_pods: no body", id="a-colon"),
        pytest.param(f"[{LIST_PODS}]", "[list_pods]", id="a-closing-bracket"),
        pytest.param(f'"{LIST_PODS}"', '"list_pods"', id="a-quote"),
        # A spelling that only opens a longer identifier names no tool
        # the runtime gave out, and the whole run stands.
        pytest.param(f"blocked {LIST_PODS}/response", f"blocked {LIST_PODS}/response", id="a-longer-path"),
        pytest.param(f"{LIST_PODS}.json", f"{LIST_PODS}.json", id="a-dotted-suffix"),
        pytest.param(f"{LIST_PODS}x", f"{LIST_PODS}x", id="a-longer-last-segment"),
        pytest.param(f"x{LIST_PODS}", f"x{LIST_PODS}", id="a-longer-first-segment"),
        pytest.param(f"notes/{LIST_PODS}", f"notes/{LIST_PODS}", id="preceded-by-a-path"),
        pytest.param(f"a/{LIST_PODS}/b", f"a/{LIST_PODS}/b", id="inside-a-longer-identifier"),
        # A spelling of the right shape this inventory never gave out.
        pytest.param("mcp:other/list_pods", "mcp:other/list_pods", id="never-issued"),
    ],
)
def test_despell_replaces_a_whole_spelling_and_leaves_every_longer_identifier(text, expected):
    built = inventory(
        http_tools=[DEMO_TOOLS],
        remote_agents=[{"name": "kagent__NS__log_analyst", "url": "http://x"}],
    )
    assert built.despell(text) == expected


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
