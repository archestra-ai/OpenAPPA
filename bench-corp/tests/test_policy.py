"""Policy pruning against the real demo policies."""

from __future__ import annotations

import tomllib

import pytest

from bench_corp.agents import AGENTS
from bench_corp.cli import SCENARIOS_DIR
from bench_corp.policy import REQUIRED_SYSTEMS_OF_TOOL, PolicyError, apply_tool_requires, prune_policy
from bench_corp.scenario import load_scenario


def _tool_names(policy_toml: str) -> set[str]:
    return {tool["name"] for tool in tomllib.loads(policy_toml).get("tool", [])}


@pytest.mark.parametrize("agent_name", ["appa", "appa-open"])
def test_demo_policies_cover_the_complete_surface(agent_name: str) -> None:
    policy = AGENTS[agent_name].policy_file.read_text()
    assert _tool_names(policy) == set(REQUIRED_SYSTEMS_OF_TOOL)


@pytest.mark.parametrize("agent_name", ["appa", "appa-open"])
def test_prune_keeps_only_enabled_systems(agent_name: str) -> None:
    policy = AGENTS[agent_name].policy_file.read_text()
    pruned = prune_policy(policy, ("hr", "email"))
    assert _tool_names(pruned) == {"search_hr", "read_hr", "create_hr", "send_email"}
    # Everything but the tool list survives the round trip — for the fork
    # policy that includes the sanitizer, boundary, and preamble tables its
    # child-return declassification depends on.
    original, result = tomllib.loads(policy), tomllib.loads(pruned)
    assert {k: v for k, v in result.items() if k != "tool"} == {k: v for k, v in original.items() if k != "tool"}


def test_prune_preserves_tool_annotations() -> None:
    policy = AGENTS["appa"].policy_file.read_text()
    pruned = tomllib.loads(prune_policy(policy, ("hr", "public_forum", "email")))
    by_name = {tool["name"]: tool for tool in pruned["tool"]}
    assert by_name["read_public_forum"]["delta"] == {"trust": "suspicious"}
    assert by_name["read_hr"]["delta"] == {"audience": {"exactly": ["hr"]}}
    assert by_name["send_email"]["requires"]["trust"] == "internal"
    assert by_name["send_email"]["effects"] == ["egress"]


def test_unknown_tool_in_policy_is_refused() -> None:
    with pytest.raises(PolicyError, match="mystery_tool"):
        prune_policy('[[tool]]\nname = "mystery_tool"\n', ("hr",))


@pytest.mark.parametrize(
    ("systems", "expected"),
    [
        (("finance",), {"search_finance", "read_finance", "create_finance"}),
        (("email",), {"send_email"}),
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
    policy = "\n".join(f'[[tool]]\nname = "{name}"' for name in REQUIRED_SYSTEMS_OF_TOOL)
    assert _tool_names(prune_policy(policy, systems)) == expected


def test_shared_policy_gates_the_ticket_on_trust_alone() -> None:
    """The prior-egress gate is one scenario's posture, not every episode's tax."""
    policy = tomllib.loads(AGENTS["appa"].policy_file.read_text())
    ticket = next(tool for tool in policy["tool"] if tool["name"] == "create_task_tracker")
    assert ticket["requires"] == {"trust": "internal"}


def test_scenario_requires_override_reaches_the_pruned_policy() -> None:
    scenario = load_scenario(SCENARIOS_DIR / "reply-and-file-ticket")
    pruned = prune_policy(AGENTS["appa"].policy_file.read_text(), scenario.systems)
    applied = apply_tool_requires(pruned, scenario.policy_requires["appa"])

    by_name = {tool["name"]: tool for tool in tomllib.loads(applied)["tool"]}
    assert by_name["create_task_tracker"]["requires"] == {
        "trust": "internal",
        "effects": {"has": ["egress"]},
    }
    # The override is scoped to the tool it names and to the arm that declared
    # it: the sibling emitter and the open baseline are untouched.
    assert by_name["create_public_forum"]["requires"] == {"audience": {"includes": ["public"]}}
    assert scenario.policy_requires.get("open") is None


def test_scenarios_without_an_override_pay_no_gate() -> None:
    scenario = load_scenario(SCENARIOS_DIR / "check-hr-record")
    pruned = prune_policy(AGENTS["appa"].policy_file.read_text(), scenario.systems)
    applied = apply_tool_requires(pruned, scenario.policy_requires.get("appa", {}))

    by_name = {tool["name"]: tool for tool in tomllib.loads(applied)["tool"]}
    assert by_name["create_task_tracker"]["requires"] == {"trust": "internal"}


def test_overriding_an_absent_tool_is_refused() -> None:
    pruned = prune_policy(AGENTS["appa"].policy_file.read_text(), ("hr", "email"))
    with pytest.raises(PolicyError, match="create_task_tracker"):
        apply_tool_requires(pruned, {"create_task_tracker": {"trust": "internal"}})
