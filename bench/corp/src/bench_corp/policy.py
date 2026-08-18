"""Per-episode APPA policy pruning and external binding.

The APPA SDK's ``bind_tools`` requires the advertised tool surface to match the
policy registry exactly (and remedy planning searches every registry tool), so
a scenario that narrows the server's ``--systems`` surface needs a policy
narrowed the same way. The bench prunes the shipped policy rather than relaxing
the SDK: parse the TOML, keep only the ``[[policy.tool]]`` entries whose system
is enabled, leave everything else untouched, and write the result into the
episode directory.

A bench policy is a deployment file: ``[policy]`` holds the policy proper and
``[externals]`` names who implements each registered authority, sanitizer, and
the dynamic resolver. The two are separate on purpose — the policy says what a
component may do, ``[externals]`` says who performs it.
"""

from __future__ import annotations

import tomllib
from functools import lru_cache

import tomli_w

from .checks import KNOWN_SYSTEMS

# The systems each tool requires. Most tools belong to one system; composite
# tools stay available only when their complete backing surface is enabled.
# ``fork`` requires none: branching is the runtime's own mechanism, registered
# as an ordinary tool, and it survives every narrowing of the corp surface.
REQUIRED_SYSTEMS_OF_TOOL: dict[str, frozenset[str]] = {
    f"{verb}_{system}": frozenset({system})
    for system in KNOWN_SYSTEMS
    if system != "email"
    for verb in ("search", "read", "create")
} | {
    "send_email": frozenset({"email"}),
    "share_legal_packet": frozenset({"finance", "email"}),
    "fork": frozenset(),
}

# A shipped policy names its externals on loopback port 0: a loadable URL that
# no listener can own. The path identifies the external to whoever hosts it, so
# binding replaces the origin and keeps the path. Two hosts can therefore bind
# the same file in turn — the bench serves the externals its scenario answers
# declare, the agent serves the rest — because each rewrites only what is still
# unbound.
UNBOUND_ORIGIN = "http://127.0.0.1:0"


class PolicyError(ValueError):
    """The policy does not have the shape this episode needs."""


def _policy_of(data: dict) -> dict:
    """The ``[policy]`` table of a deployment file."""
    policy = data.get("policy")
    if not isinstance(policy, dict):
        raise PolicyError("policy file has no [policy] table; a bench policy is a deployment file")
    return policy


@lru_cache(maxsize=None)  # each (policy, systems) pair is pruned once, not per rep
def prune_policy(policy_toml: str, enabled_systems: tuple[str, ...]) -> str:
    """The policy text with only the enabled systems' ``[[policy.tool]]`` entries."""
    data = tomllib.loads(policy_toml)
    policy = _policy_of(data)
    enabled = set(enabled_systems)
    kept = []
    for tool in policy.get("tool", []):
        name = tool.get("name", "")
        required = REQUIRED_SYSTEMS_OF_TOOL.get(name)
        if required is None:
            raise PolicyError(
                f"policy declares tool {name!r} with no known systems; extend REQUIRED_SYSTEMS_OF_TOOL"
            )
        if required <= enabled:
            kept.append(tool)
    policy["tool"] = kept

    # The deployment's coverage slots reference tools by name, and naming an
    # unregistered tool is refused at load, so a pruned tool leaves the
    # deployment too.
    deployment = policy.get("deployment")
    if isinstance(deployment, dict):
        surviving = {tool["name"] for tool in kept}
        deployment["confined_results"] = [
            name for name in deployment.get("confined_results", []) if name in surviving
        ]
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
    by_name = {tool.get("name", ""): tool for tool in _policy_of(data).get("tool", [])}
    for name, requires in overrides.items():
        tool = by_name.get(name)
        if tool is None:
            raise PolicyError(f"scenario overrides requires of tool {name!r}, absent from the pruned policy")
        tool["requires"] = requires
    return tomli_w.dumps(data)


def bind_external_urls(policy_toml: str, origin: str) -> str:
    """Point every still-unbound ``[externals]`` endpoint at ``origin``.

    The origin changes per episode because the server binds an ephemeral port;
    the path is what the server routes on, so it survives.
    """
    data = tomllib.loads(policy_toml)
    externals = data.get("externals", {})
    bound = 0
    endpoints = [externals.get("dynamic")]
    for kind in ("authorities", "sanitizers"):
        endpoints.extend(externals.get(kind, {}).values())
    for endpoint in endpoints:
        if not isinstance(endpoint, dict):
            continue
        url = endpoint.get("url", "")
        if url.startswith(f"{UNBOUND_ORIGIN}/"):
            endpoint["url"] = origin.rstrip("/") + url.removeprefix(UNBOUND_ORIGIN)
            bound += 1
    if bound == 0:
        raise PolicyError("scenario declares external answers but its policy has no unbound endpoint")
    return tomli_w.dumps(data)
