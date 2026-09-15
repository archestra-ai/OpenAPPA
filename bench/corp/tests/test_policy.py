"""Policy pruning and external binding against the real shipped policies."""

from __future__ import annotations

import tomllib
import json
import hashlib

import pytest

from bench_corp.agents import AGENTS
from bench_corp.auto_policy import FACTS, settings_for
from bench_corp.cli import SCENARIOS_DIR
from bench_corp.policy import (
    REQUIRED_SYSTEMS_OF_TOOL,
    UNBOUND_ORIGIN,
    PolicyError,
    apply_tool_requires,
    bind_external_urls,
    prune_policy,
)
from bench_corp.scenario import load_scenario


def _tools_of(policy_toml: str) -> list[dict]:
    return tomllib.loads(policy_toml)["policy"].get("tool", [])


def _tool_names(policy_toml: str) -> set[str]:
    return {tool["name"] for tool in _tools_of(policy_toml)}


@pytest.mark.parametrize("agent_name", ["appa", "appa-open"])
def test_demo_policies_cover_the_complete_surface(agent_name: str) -> None:
    policy = AGENTS[agent_name].policy_file.read_text()
    assert _tool_names(policy) == set(REQUIRED_SYSTEMS_OF_TOOL)


@pytest.mark.parametrize("agent_name", ["appa", "appa-open"])
def test_prune_keeps_only_enabled_systems(agent_name: str) -> None:
    policy = AGENTS[agent_name].policy_file.read_text()
    pruned = prune_policy(policy, ("hr", "email"))
    assert _tool_names(pruned) == {"search_hr", "read_hr", "create_hr", "send_email", "fork"}

    # Everything the tool list does not reach survives the round trip — for the
    # fork policy that includes the sanitizer and boundary tables its
    # child-return declassification depends on, and the registered externals.
    original, result = tomllib.loads(policy), tomllib.loads(pruned)
    assert {k: v for k, v in result.items() if k != "policy"} == {
        k: v for k, v in original.items() if k != "policy"
    }
    untouched = {"tool", "deployment"}
    assert {k: v for k, v in result["policy"].items() if k not in untouched} == {
        k: v for k, v in original["policy"].items() if k not in untouched
    }


@pytest.mark.parametrize("agent_name", ["appa", "appa-open"])
def test_prune_drops_the_pruned_tools_from_the_deployment(agent_name: str) -> None:
    """Naming an unregistered tool in a coverage slot is refused at load."""
    pruned = prune_policy(AGENTS[agent_name].policy_file.read_text(), ("hr", "email"))
    deployment = tomllib.loads(pruned)["policy"]["deployment"]
    # `fork` produces no result to confine: releasing it opens a child, and
    # what the child returns is checked at the merge instead.
    assert set(deployment["confined_results"]) == _tool_names(pruned) - {"fork"}


def test_prune_preserves_tool_annotations() -> None:
    policy = AGENTS["appa"].policy_file.read_text()
    by_name = {tool["name"]: tool for tool in _tools_of(prune_policy(policy, ("hr", "public_forum", "email")))}
    assert by_name["read_public_forum"]["delta"] == {"trust": "suspicious"}
    assert by_name["read_hr"]["delta"] == {"audience": ["hr"]}
    assert by_name["send_email"]["requires"]["trust"] == "internal"
    assert by_name["send_email"]["effects"] == ["egress"]


def test_undeclared_tool_in_policy_is_refused() -> None:
    with pytest.raises(PolicyError, match="mystery_tool"):
        prune_policy('[policy]\n[[policy.tool]]\nname = "mystery_tool"\n', ("hr",))


def test_a_pure_policy_file_is_refused() -> None:
    """The bench runs deployments; a policy with no [policy] table is not one."""
    with pytest.raises(PolicyError, match=r"\[policy\] table"):
        prune_policy('version = 1\n[[tool]]\nname = "send_email"\n', ("email",))


@pytest.mark.parametrize(
    ("systems", "expected"),
    [
        (("finance",), {"search_finance", "read_finance", "create_finance"}),
        (("email",), {"send_email"}),
        (("wire",), {"execute_wire"}),
        (
            ("finance", "email"),
            {"search_finance", "read_finance", "create_finance", "send_email", "share_legal_packet"},
        ),
        (("vendor",), {"search_vendor", "read_vendor", "create_vendor"}),
    ],
)
def test_prune_keeps_tools_only_when_all_required_systems_are_enabled(
    systems: tuple[str, ...], expected: set[str]
) -> None:
    # Branching is the runtime's own mechanism registered as a tool, so it
    # requires no system and survives every narrowing of the corp surface.
    expected = expected | {"fork"}
    policy = "[policy]\n" + "\n".join(
        f'[[policy.tool]]\nname = "{name}"' for name in REQUIRED_SYSTEMS_OF_TOOL
    )
    assert _tool_names(prune_policy(policy, systems)) == expected


def test_shared_policy_gates_the_ticket_on_trust_and_destination_audience() -> None:
    """Persistent writes must not launder content into a wider-reader store."""
    tools = _tools_of(AGENTS["appa"].policy_file.read_text())
    ticket = next(tool for tool in tools if tool["name"] == "create_task_tracker")
    assert ticket["requires"] == {
        "trust": "internal",
        "audience": {"contains": ["public"]},
    }


def test_scenario_requires_override_reaches_the_pruned_policy() -> None:
    scenario = load_scenario(SCENARIOS_DIR / "reply-and-file-ticket")
    pruned = prune_policy(AGENTS["appa"].policy_file.read_text(), scenario.systems)
    applied = apply_tool_requires(pruned, scenario.policy_requires["appa"])

    by_name = {tool["name"]: tool for tool in _tools_of(applied)}
    assert by_name["create_task_tracker"]["requires"] == {
        "trust": "internal",
        "audience": {"contains": ["public"]},
        "effects": {"contains": ["egress"]},
    }
    # The override is scoped to the tool it names and to the arm that declared
    # it: the sibling emitter and the open baseline are untouched.
    assert by_name["create_public_forum"]["requires"] == {"audience": {"contains": ["public"]}}
    assert scenario.policy_requires.get("open") is None


def test_scenarios_without_an_override_keep_the_shared_sink_gate() -> None:
    scenario = load_scenario(SCENARIOS_DIR / "check-hr-record")
    pruned = prune_policy(AGENTS["appa"].policy_file.read_text(), scenario.systems)
    applied = apply_tool_requires(pruned, scenario.policy_requires.get("appa", {}))

    by_name = {tool["name"]: tool for tool in _tools_of(applied)}
    assert by_name["create_task_tracker"]["requires"] == {
        "trust": "internal",
        "audience": {"contains": ["public"]},
    }


def test_overriding_an_absent_tool_is_refused() -> None:
    pruned = prune_policy(AGENTS["appa"].policy_file.read_text(), ("hr", "email"))
    with pytest.raises(PolicyError, match="create_task_tracker"):
        apply_tool_requires(pruned, {"create_task_tracker": {"trust": "internal"}})


@pytest.mark.parametrize(
    ("scenario_name", "path"),
    [
        ("dual-control-wire", "/authority/secondary-wire-approver"),
        ("blind-promotion", "/sanitizer/demographics-redactor"),
        ("route-project-packet", "/annotator"),
    ],
)
def test_binding_replaces_the_origin_and_keeps_the_path(scenario_name: str, path: str) -> None:
    """The server routes on the path; only the ephemeral origin is per-episode."""
    policy = (SCENARIOS_DIR / scenario_name / "policy" / "appa.toml").read_text()
    bound = bind_external_urls(prune_policy(policy, ("finance", "email", "hr", "vendor")), "http://127.0.0.1:12345/")

    assert f"http://127.0.0.1:12345{path}" in bound
    assert UNBOUND_ORIGIN not in bound


def test_binding_requires_an_unbound_endpoint() -> None:
    already_bound = (
        "[policy]\n[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n"
        '[externals.authorities.real]\nurl = "https://approval.example"\n'
    )
    with pytest.raises(PolicyError, match="no unbound endpoint"):
        bind_external_urls(already_bound, "http://127.0.0.1:12345")


def test_auto_ifc_has_valid_settings_for_every_scenario() -> None:
    scenario_names = {path.parent.name for path in SCENARIOS_DIR.glob("*/scenario.toml")}
    assert set(FACTS) == scenario_names
    for name in scenario_names:
        rendered, digest = settings_for(name)
        assert hashlib.sha256(rendered.encode()).hexdigest() == digest
        auto_mode = json.loads(rendered)["autoMode"]
        for section in ("environment", "allow", "soft_deny", "hard_deny"):
            assert "$defaults" in auto_mode[section]
        assert any("source labels" in rule.lower() for rule in auto_mode["environment"])
        assert any("sinks and audiences" in rule.lower() for rule in auto_mode["environment"])
        assert any("narrowing" in rule.lower() for rule in auto_mode["environment"])


def test_auto_ifc_overhead_uses_stock_auto_baseline() -> None:
    from dataclasses import replace
    from bench_corp.report import AgentSummary, usage_overhead

    fields = dict(episodes=1, errors=0, provider_errors=0, harness_errors=0,
        budget_finalized=0, utility_passed=1, utility_total=1, attacks_succeeded=0,
        attacks_total=1, mean_duration_s=1.0, policy_events=0, remedy_calls=0,
        provider_retries=0, model_usage_episodes=1, model_calls=1, input_tokens=10,
        output_tokens=5, total_tokens=15, cached_input_tokens=0,
        cache_write_input_tokens=0, reasoning_tokens=None, cost_usd=0.1,
        mean_total_tokens=15.0, mean_cost_usd=0.1)
    auto = AgentSummary(agent="auto", **fields)
    tuned = replace(auto, agent="auto-ifc", mean_total_tokens=18.0, mean_cost_usd=0.12)
    assert usage_overhead([auto, tuned])["auto-ifc"]["baseline"] == "auto"
