"""Scenario folders: prompt + own corpus + enabled systems + expected end state.

A scenario is a directory holding ``scenario.toml`` and ``corpus/``. The corpus
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

KNOWN_SYSTEMS = ("hr", "finance", "task_tracker", "public_forum", "email")

CHECK_KINDS = ("email_sent", "no_email", "file_created", "answer_contains")


class ScenarioError(ValueError):
    """A scenario folder is malformed; the message names the scenario and why."""


@dataclass(frozen=True)
class Check:
    """One end-state predicate: a kind from :data:`CHECK_KINDS` plus its spec
    table, validated field-by-field in :mod:`.checks` at load time."""

    kind: str
    spec: dict


@dataclass(frozen=True)
class Scenario:
    name: str
    root: Path  # the scenario folder itself
    prompt: str
    systems: tuple[str, ...]
    utility: tuple[Check, ...] = field(default=())
    security: tuple[Check, ...] = field(default=())

    @property
    def corpus(self) -> Path:
        return self.root / "corpus"


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


def load_scenario(root: Path) -> Scenario:
    """Load and validate one scenario folder.

    Validation is deliberately strict: a scenario must not be able to declare
    an expected outcome its own tool surface or corpus cannot produce.
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

    utility = _checks_of(name, "utility", data.get("utility", {}))
    security = _checks_of(name, "security", data.get("security", {}))
    if not utility and not security:
        raise ScenarioError(f"{name}: declare at least one utility or security check")

    scenario = Scenario(
        name=name,
        root=root,
        prompt=prompt.strip(),
        systems=tuple(systems),
        utility=utility,
        security=security,
    )

    corpus = scenario.corpus
    if not corpus.is_dir():
        raise ScenarioError(f"{name}: no corpus/ directory in {root}")
    for entry in sorted(corpus.iterdir()):
        if entry.name == "email":
            raise ScenarioError(f"{name}: corpus must not contain email/ — the sink is per-episode, not corpus data")
        if entry.is_dir() and entry.name not in systems:
            raise ScenarioError(f"{name}: corpus dir {entry.name}/ is not in 'systems' ({', '.join(systems)})")

    for section, checks in (("utility", utility), ("security", security)):
        for check in checks:
            needed = _system_of_check(check)
            if needed is not None and needed not in systems:
                raise ScenarioError(
                    f"{name}: {section}.{check.kind} needs the {needed!r} system, which is not in 'systems'"
                )

    # Field-level check validation happens once here, not per episode.
    from .checks import validate_check  # local import to keep module deps one-way

    for section, checks in (("utility", utility), ("security", security)):
        for check in checks:
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
