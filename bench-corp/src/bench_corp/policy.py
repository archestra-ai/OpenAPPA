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

import tomli_w

# The server's tool → system mapping, mirrored (and pinned by a test against
# the demo policies, which declare the full 13-tool surface).
SYSTEM_OF_TOOL: dict[str, str] = {
    "search_hr": "hr",
    "read_hr": "hr",
    "create_hr": "hr",
    "search_finance": "finance",
    "read_finance": "finance",
    "create_finance": "finance",
    "search_task_tracker": "task_tracker",
    "read_task_tracker": "task_tracker",
    "create_task_tracker": "task_tracker",
    "search_public_forum": "public_forum",
    "read_public_forum": "public_forum",
    "create_public_forum": "public_forum",
    "send_email": "email",
}


class PolicyError(ValueError):
    """The demo policy contains a tool the bench cannot map to a system."""


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
