"""Policy pruning against the real demo policies."""

from __future__ import annotations

import tomllib

import pytest

from bench_corp.policy import SYSTEM_OF_TOOL, PolicyError, prune_policy
from bench_corp.agents import AGENTS


def _tool_names(policy_toml: str) -> set[str]:
    return {tool["name"] for tool in tomllib.loads(policy_toml).get("tool", [])}


@pytest.mark.parametrize("agent_name", ["appa", "appa-open"])
def test_demo_policies_cover_exactly_the_known_surface(agent_name: str) -> None:
    policy = AGENTS[agent_name].policy_file.read_text()
    assert _tool_names(policy) == set(SYSTEM_OF_TOOL)


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
