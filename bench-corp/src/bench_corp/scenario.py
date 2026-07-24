"""Scenario folders: prompt + own data + enabled systems + expected end state.

A scenario is a directory holding ``scenario.toml`` and ``data/``. The data
is the complete world the episode plays out in — attack files included, sitting
exactly where the tools read them — so scenarios never share state and editing
one cannot affect another. The TOML declares the user prompt, which systems the
MCP server keeps enabled, and the end-state checks:

* ``[[utility.<check>]]``  — the user's goal happened;
* ``[[security.<check>]]`` — the attacker's goal happened (attack success).

Check vocabulary (see :mod:`.checks`): ``email_sent``, ``no_email``,
``file_created``, ``answer_contains``.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from pathlib import Path

from .checks import CHECK_KINDS, KNOWN_SYSTEMS, Check, validate_check


class ScenarioError(ValueError):
    """A scenario folder is malformed; the message names the scenario and why."""


@dataclass(frozen=True)
class PolicyProfile:
    appa: Path
    fides: Path


@dataclass(frozen=True)
class Scenario:
    name: str
    root: Path  # the scenario folder itself
    prompt: str
    systems: tuple[str, ...]
    policy_profile: PolicyProfile | None = None
    utility: tuple[Check, ...] = field(default=())
    security: tuple[Check, ...] = field(default=())
    # Extra `requires` this scenario's deployment puts on a tool, keyed by
    # policy file stem then tool name (`[policy.appa.requires]`). A gate only
    # one scenario exercises belongs to that scenario, not to every episode
    # that happens to share the policy — see `policy.apply_tool_requires`.
    policy_requires: dict[str, dict[str, dict]] = field(default_factory=dict)

    @property
    def data(self) -> Path:
        return self.root / "data"


def _checks_of(name: str, section: str, table: dict) -> tuple[Check, ...]:
    checks: list[Check] = []
    for kind, specs in table.items():
        if kind not in CHECK_KINDS:
            raise ScenarioError(f"{name}: unknown {section} check kind {kind!r}; valid: {', '.join(CHECK_KINDS)}")
        if not isinstance(specs, list):
            raise ScenarioError(f"{name}: {section}.{kind} must be an array of tables ([[{section}.{kind}]])")
        checks.extend(Check(kind=kind, spec=spec) for spec in specs)
    return tuple(checks)


def _system_of_check(check: Check) -> str | None:
    """The system a check's outcome depends on, for enablement validation."""
    if check.kind in ("email_sent", "no_email"):
        return "email"
    if check.kind == "file_created":
        return check.spec.get("system")
    return None  # answer_contains reads stdout, no tool needed


def _load_policy_profile(name: str, root: Path, value: object) -> PolicyProfile:
    if not isinstance(value, str):
        raise ScenarioError(f"{name}: 'policy_profile' must be a string")
    if not value.strip():
        raise ScenarioError(f"{name}: 'policy_profile' must not be empty")

    relative = Path(value)
    if relative.is_absolute():
        raise ScenarioError(f"{name}: 'policy_profile' must be relative to the scenario directory")
    if ".." in relative.parts:
        raise ScenarioError(f"{name}: 'policy_profile' must not contain '..'")

    scenario_root = root.resolve()
    try:
        profile_root = (root / relative).resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise ScenarioError(f"{name}: policy profile directory {value!r} does not exist") from error
    if not profile_root.is_relative_to(scenario_root):
        raise ScenarioError(f"{name}: policy profile {value!r} escapes the scenario directory")
    if not profile_root.is_dir():
        raise ScenarioError(f"{name}: policy profile {value!r} is not a directory")

    files: dict[str, Path] = {}
    for target, filename in (("appa", "appa.toml"), ("fides", "fides.json")):
        path = profile_root / filename
        try:
            resolved = path.resolve(strict=True)
        except (OSError, RuntimeError) as error:
            raise ScenarioError(f"{name}: policy profile requires {filename}") from error
        if not resolved.is_relative_to(profile_root):
            raise ScenarioError(f"{name}: policy profile file {filename} escapes the policy profile directory")
        if not resolved.is_file():
            raise ScenarioError(f"{name}: policy profile requires {filename} to be a file")
        files[target] = resolved
    return PolicyProfile(appa=files["appa"], fides=files["fides"])


def _policy_requires_of(name: str, table: dict) -> dict[str, dict[str, dict]]:
    """Parse ``[policy.<policy-stem>.requires]``: per-tool `requires` this
    scenario adds to that policy. The tool names are checked against the pruned
    policy at episode setup, where the policy is in hand."""
    if not isinstance(table, dict):
        raise ScenarioError(f"{name}: 'policy' must be a table of policy-stem tables")
    parsed: dict[str, dict[str, dict]] = {}
    for stem, body in table.items():
        if not isinstance(body, dict) or set(body) - {"requires"}:
            raise ScenarioError(f"{name}: policy.{stem} takes exactly one key, 'requires'")
        requires = body.get("requires", {})
        if not isinstance(requires, dict) or not requires:
            raise ScenarioError(f"{name}: policy.{stem}.requires must be a non-empty table keyed by tool name")
        for tool, spec in requires.items():
            if not isinstance(spec, dict) or not spec:
                raise ScenarioError(f"{name}: policy.{stem}.requires.{tool} must be a non-empty table")
        parsed[stem] = requires
    return parsed


def load_scenario(root: Path) -> Scenario:
    """Load and validate one scenario folder.

    Validation is deliberately strict: a scenario must not be able to declare
    an expected outcome its own tool surface or data cannot produce.
    """
    name = root.name
    manifest = root / "scenario.toml"
    if not manifest.is_file():
        raise ScenarioError(f"{name}: no scenario.toml in {root}")
    data = tomllib.loads(manifest.read_text())

    prompt = data.get("prompt", "")
    if not isinstance(prompt, str) or not prompt.strip():
        raise ScenarioError(f"{name}: 'prompt' must be a non-empty string")

    systems = data.get("systems")
    if not isinstance(systems, list) or not systems:
        raise ScenarioError(f"{name}: 'systems' must be a non-empty list")
    for system in systems:
        if system not in KNOWN_SYSTEMS:
            raise ScenarioError(f"{name}: unknown system {system!r}; valid: {', '.join(KNOWN_SYSTEMS)}")
    if len(set(systems)) != len(systems):
        raise ScenarioError(f"{name}: duplicate entries in 'systems'")

    policy_profile = None
    if "policy_profile" in data:
        policy_profile = _load_policy_profile(name, root, data["policy_profile"])

    utility = _checks_of(name, "utility", data.get("utility", {}))
    security = _checks_of(name, "security", data.get("security", {}))
    if not utility and not security:
        raise ScenarioError(f"{name}: declare at least one utility or security check")

    scenario = Scenario(
        name=name,
        root=root,
        prompt=prompt.strip(),
        systems=tuple(systems),
        policy_profile=policy_profile,
        utility=utility,
        security=security,
        policy_requires=_policy_requires_of(name, data.get("policy", {})),
    )

    data_dir = scenario.data
    if not data_dir.is_dir():
        raise ScenarioError(f"{name}: no data/ directory in {root}")
    for entry in sorted(data_dir.iterdir()):
        if entry.name == "email":
            raise ScenarioError(f"{name}: data/ must not contain email/ — the sink is per-episode, not scenario data")
        if entry.is_dir() and entry.name not in systems:
            raise ScenarioError(f"{name}: data dir {entry.name}/ is not in 'systems' ({', '.join(systems)})")

    # Per-check validation happens once here, not per episode: the check's
    # fields must be well-formed and its outcome producible by the enabled
    # tool surface.
    for section, checks in (("utility", utility), ("security", security)):
        for check in checks:
            needed = _system_of_check(check)
            if needed is not None and needed not in systems:
                raise ScenarioError(
                    f"{name}: {section}.{check.kind} needs the {needed!r} system, which is not in 'systems'"
                )
            try:
                validate_check(check)
            except ValueError as error:
                raise ScenarioError(f"{name}: bad {section}.{check.kind}: {error}") from error

    return scenario


def discover_scenarios(scenarios_dir: Path, names: list[str] | None = None) -> list[Scenario]:
    """Load all (or the named) scenario folders under ``scenarios_dir``, sorted."""
    if names:
        roots = []
        for name in names:
            root = scenarios_dir / name
            if not root.is_dir():
                have = ", ".join(sorted(p.name for p in scenarios_dir.iterdir() if p.is_dir()))
                raise ScenarioError(f"no scenario named {name!r} under {scenarios_dir}; have: {have}")
            roots.append(root)
    else:
        roots = sorted(p for p in scenarios_dir.iterdir() if p.is_dir())
    return [load_scenario(root) for root in roots]
