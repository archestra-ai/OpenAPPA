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

# The server's tool → system mapping — the `{verb}_{system}` naming convention
# plus the one sink, pinned by a test against the demo policies (which declare
# the full 13-tool surface).
SYSTEM_OF_TOOL: dict[str, str] = {
    f"{verb}_{system}": system
    for system in KNOWN_SYSTEMS
    if system != "email"
    for verb in ("search", "read", "create")
} | {"send_email": "email"}


class PolicyError(ValueError):
    """The demo policy contains a tool the bench cannot map to a system."""


@lru_cache(maxsize=None)  # each (policy, systems) pair is pruned once, not per rep
def prune_policy(policy_toml: str, enabled_systems: tuple[str, ...]) -> str:
    """The policy text with only the enabled systems' ``[[tool]]`` entries."""
    data = tomllib.loads(policy_toml)
    kept = []
    for tool in data.get("tool", []):
        name = tool.get("name", "")
        system = SYSTEM_OF_TOOL.get(name)
        if system is None:
            raise PolicyError(f"policy declares tool {name!r} with no known system; extend SYSTEM_OF_TOOL")
        if system in enabled_systems:
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
