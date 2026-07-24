"""Per-episode APPA policy pruning.

The APPA SDK's ``bind_tools`` requires the advertised tool surface to match the
policy registry exactly (and remedy planning searches every registry tool), so
a scenario that narrows the server's ``--systems`` surface needs a policy
narrowed the same way. The bench prunes the demo's policy rather than relaxing
the SDK: parse the TOML, keep only the ``[[tool]]`` entries whose system is
enabled, leave everything else (version, trust_chain, authorities) untouched,
and write the result into the episode directory for ``--policy``.
"""

from __future__ import annotations

import tomllib
from functools import lru_cache

import tomli_w

from .checks import KNOWN_SYSTEMS

# The systems each tool requires. Most tools belong to one system; composite
# tools stay available only when their complete backing surface is enabled.
REQUIRED_SYSTEMS_OF_TOOL: dict[str, frozenset[str]] = {
    f"{verb}_{system}": frozenset({system})
    for system in KNOWN_SYSTEMS
    if system != "email"
    for verb in ("search", "read", "create")
} | {
    "send_email": frozenset({"email"}),
    "share_legal_packet": frozenset({"finance", "email"}),
}


class PolicyError(ValueError):
    """The demo policy contains a tool the bench cannot map to a system."""


@lru_cache(maxsize=None)  # each (policy, systems) pair is pruned once, not per rep
def prune_policy(policy_toml: str, enabled_systems: tuple[str, ...]) -> str:
    """The policy text with only the enabled systems' ``[[tool]]`` entries."""
    data = tomllib.loads(policy_toml)
    enabled = set(enabled_systems)
    kept = []
    for tool in data.get("tool", []):
        name = tool.get("name", "")
        required = REQUIRED_SYSTEMS_OF_TOOL.get(name)
        if required is None:
            raise PolicyError(
                f"policy declares tool {name!r} with no known systems; extend REQUIRED_SYSTEMS_OF_TOOL"
            )
        if required <= enabled:
            kept.append(tool)
    data["tool"] = kept
    return tomli_w.dumps(data)


def apply_tool_requires(policy_toml: str, overrides: dict[str, dict]) -> str:
    """The policy text with each named tool's ``requires`` replaced.

    A requirement only one scenario exercises is that scenario's deployment
    posture, not the bench's: carrying it in the shared policy taxes every
    other episode with a gate it never meant to test, and makes a failure
    ambiguous between the mechanism under test and the tax.
    """
    if not overrides:
        return policy_toml
    data = tomllib.loads(policy_toml)
    by_name = {tool.get("name", ""): tool for tool in data.get("tool", [])}
    for name, requires in overrides.items():
        tool = by_name.get(name)
        if tool is None:
            raise PolicyError(f"scenario overrides requires of tool {name!r}, absent from the pruned policy")
        tool["requires"] = requires
    return tomli_w.dumps(data)
