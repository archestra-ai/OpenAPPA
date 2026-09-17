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

import json
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
class AnnotatorAnswer:
    """One exact annotation consult answered by the scenario's loopback annotator.

    The consult carries no tool name, so the annotator and the exact `args` it was sent are
    the whole key. `args` is held in its canonical JSON spelling, which is what the fixture
    compares an arriving consult against; `annotation` is the verbatim wire annotation.
    """

    annotator: str
    args: str
    annotation: dict

    @property
    def request_key(self) -> tuple[str, str]:
        return (self.annotator, self.args)


def canonical_args(args: object) -> str:
    """The one spelling of a request's `args`, so declaration and request compare equal."""
    return json.dumps(args, sort_keys=True, separators=(",", ":"))


@dataclass(frozen=True)
class AuthorityAnswer:
    """One authority ruling served by the scenario's loopback fixture."""

    authority: str
    tool: str
    ruling: str


@dataclass(frozen=True)
class SanitizerAnswer:
    """One deterministic line-dropping sanitizer hosted by the fixture."""

    sanitizer: str
    drop_lines_containing: tuple[str, ...]


@dataclass(frozen=True)
class Scenario:
    name: str
    root: Path  # the scenario folder itself
    prompt: str
    systems: tuple[str, ...]
    policy_profile: PolicyProfile | None = None
    annotator_answers: tuple[AnnotatorAnswer, ...] = field(default=())
    authority_answers: tuple[AuthorityAnswer, ...] = field(default=())
    sanitizer_answers: tuple[SanitizerAnswer, ...] = field(default=())
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


def _systems_of_check(check: Check) -> tuple[str, ...]:
    """The systems a check's outcome depends on, for enablement validation."""
    if check.kind in ("email_sent", "no_email"):
        return ("email",)
    if check.kind in ("file_created", "file_created_without_authority"):
        system = check.spec.get("system")
        return (system,) if system is not None else ()
    if check.kind in ("file_created_before_email", "email_before_file_created"):
        system = check.spec.get("system")
        return ("email", system) if system is not None else ("email",)
    if check.kind in ("wire_executed", "wire_executed_with_authority", "wire_executed_without_authority"):
        return ("wire",)
    return ()  # answer_contains and authority_called need no corporate system


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


@dataclass(frozen=True)
class DeclaredAnnotator:
    """One annotator's declaration, as the scenario's policy writes it: the input names its
    consults carry and each mandate bound, `None` where the policy leaves it unbounded."""

    inputs: frozenset[str]
    ranks: frozenset[str] | None
    audiences: frozenset[str] | None
    marks: frozenset[str] | None
    effects: frozenset[str] | None


def _mandate_bound(declaration: dict, key: str) -> frozenset[str] | None:
    values = declaration.get(key)
    return None if values is None else frozenset(values)


def _audience_bound(declaration: dict, name: str) -> frozenset[str] | None:
    """The written `audiences` bound in the spellings the runtime's declaration exposes: a
    literal reader canonical, a chain word or `@` mention as written."""
    match declaration.get("audiences"):
        case None:
            return None
        case list() as entries if all(isinstance(entry, str) for entry in entries):
            return frozenset(_canonical_reader(entry) for entry in entries)
        case _:
            raise ScenarioError(
                f"{name}: annotator {declaration.get('name')!r} audiences must be an array of strings"
            )


def _canonical_reader(entry: str) -> str:
    """The engine's one spelling per reader: an address keeps its local part and lowercases
    its domain; every other spelling — a chain word, an `@` mention, an opaque id — is
    untouched."""
    local, at, domain = entry.partition("@")
    malformed = (
        not at
        or not local
        or not domain
        or "@" in domain
        or any(char.isspace() or not char.isprintable() for char in entry)
    )
    return entry if malformed else f"{local}@{domain.lower()}"


def _declared_annotators(name: str, policy: Path) -> dict[str, DeclaredAnnotator]:
    """Each annotator this scenario's APPA policy declares.

    Reading it here keeps the fixture from holding a second copy of the contract. A scenario
    that renames an annotator, answers for one the policy never declares, keys an answer on
    an input the annotator does not map, or writes an annotation outside the annotator's
    mandate, fails at load instead of at the first consult.
    """
    try:
        document = tomllib.loads(policy.read_text())
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ScenarioError(f"{name}: cannot read {policy}: {error}") from error
    section = document.get("policy", document)
    chain = section.get("trust_chain") or document.get("trust_chain")
    ranks = frozenset(chain) if isinstance(chain, list) else None
    declared = {}
    for declaration in section.get("annotator", []):
        inputs = declaration.get("inputs", {})
        if not isinstance(inputs, dict) or not inputs:
            raise ScenarioError(
                f"{name}: annotator {declaration.get('name')!r} maps no inputs; the loopback "
                "fixture keys answers on mapped inputs, so every answered annotator maps them"
            )
        declared_ranks = _mandate_bound(declaration, "ranks")
        declared[declaration["name"]] = DeclaredAnnotator(
            inputs=frozenset(inputs),
            # An unbounded rank mandate still stays inside the policy's trust chain.
            ranks=ranks if declared_ranks is None else declared_ranks,
            audiences=_audience_bound(declaration, name),
            marks=_mandate_bound(declaration, "marks"),
            effects=_mandate_bound(declaration, "effects"),
        )
    return declared


_CHAIN_WORDS = frozenset({"self", "internal"})


def _spelled_audience(entry: str) -> bool:
    """Whether one entry is a spelling the policy grammar admits: a chain word, an `@`
    mention with a name (and a selector after any `:`), or a literal reader."""
    match entry:
        case "":
            return False
        case _ if entry.startswith("$"):
            return False
        case _ if entry.startswith("@"):
            provider, colon, selector = entry[1:].partition(":")
            return bool(provider) and (not colon or bool(selector))
        case _:
            return True


def audience_entries(value: object, location: str, field: str, name: str) -> list[str]:
    """One audience value off the wire, in the engine's written-list grammar: `"public"`
    alone (naming no entry), or a non-empty array of distinct entries — at most one of
    `self`/`internal`, never `public` inside the array, no argument placeholder — each
    literal reader returned in its canonical spelling."""
    where = f"{name}: {location}.{field}"
    match value:
        case "public":
            return []
        case list() as written if written and all(isinstance(entry, str) for entry in written):
            entries = [_canonical_reader(entry) for entry in written]
        case _:
            raise ScenarioError(f"{where} is not a wire audience shape: `public` or a non-empty array")
    if "public" in entries:
        raise ScenarioError(f"{where} lists `public` inside an array")
    if len(set(entries)) != len(entries):
        raise ScenarioError(f"{where} repeats an entry")
    if len(_CHAIN_WORDS & set(entries)) > 1:
        raise ScenarioError(f"{where} names both `self` and `internal`")
    for entry in entries:
        if not _spelled_audience(entry):
            raise ScenarioError(f"{where} entry {entry!r} names no audience")
    return entries


def _audiences_of(value: object, location: str, field: str, name: str) -> list[str]:
    """Every audience entry an annotation audience field names. A `delta.audience` is a
    bare value; a `requires.audience` holds a `contains` floor, a `within` cap, or both."""
    match value:
        case dict() as bounds if bounds and set(bounds) <= {"contains", "within"}:
            return [
                entry
                for key, bound in bounds.items()
                for entry in audience_entries(bound, location, f"{field}.{key}", name)
            ]
        case _:
            return audience_entries(value, location, field, name)


def _within_mandate(
    name: str, location: str, annotation: dict, spec: DeclaredAnnotator
) -> None:
    """Refuse at load what the runtime's mandate check would refuse at the first consult."""
    delta = annotation["delta"]
    requires = annotation["requires"]
    if not isinstance(delta, dict) or not set(delta) <= {"trust", "audience"}:
        raise ScenarioError(f"{name}: {location}.annotation.delta takes only trust and audience")
    if not set(requires) <= {"trust", "audience", "history", "attention"}:
        raise ScenarioError(
            f"{name}: {location}.annotation.requires takes only trust, audience, history, attention"
        )
    for field, holder in (("delta", delta), ("requires", requires)):
        trust = holder.get("trust")
        if trust is not None and spec.ranks is not None and trust not in spec.ranks:
            raise ScenarioError(f"{name}: {location}.annotation.{field}.trust {trust!r} is outside the mandate")
        audience = holder.get("audience")
        if audience is not None:
            entries = _audiences_of(audience, location, f"annotation.{field}.audience", name)
            if spec.audiences is not None and not set(entries) <= spec.audiences:
                outside = sorted(set(entries) - spec.audiences)
                raise ScenarioError(f"{name}: {location}.annotation.{field}.audience names {outside} outside the mandate")
    emits = annotation["emits"]
    if not isinstance(emits, list):
        raise ScenarioError(f"{name}: {location}.annotation.emits must be an array")
    marks = requires["attention"]
    for field, values in (("requires.attention", marks), ("emits", emits)):
        if not all(isinstance(value, str) for value in values):
            raise ScenarioError(f"{name}: {location}.annotation.{field} entries must be strings")
    # A history entry is a one-key selector over the declared effect kinds.
    history_effects = []
    for entry in requires["history"]:
        match entry:
            case {"contains": str() as effect} | {"excludes": str() as effect} if len(entry) == 1:
                history_effects.append(effect)
            case _:
                raise ScenarioError(
                    f"{name}: {location}.annotation.requires.history entries are one-key "
                    "contains/excludes selectors over effect kinds"
                )
    if spec.marks is not None and not set(marks) <= spec.marks:
        raise ScenarioError(f"{name}: {location}.annotation.requires.attention is outside the mandate")
    if spec.effects is not None and not (set(emits) | set(history_effects)) <= spec.effects:
        raise ScenarioError(f"{name}: {location}.annotation effects are outside the mandate")


def _annotator_answers_of(
    name: str, declarations: object, declared: dict[str, DeclaredAnnotator]
) -> tuple[AnnotatorAnswer, ...]:
    """Parse the exact wire annotations served to this scenario's APPA episodes."""
    if not isinstance(declarations, list):
        raise ScenarioError(f"{name}: 'annotator_answer' must be an array of tables")
    fields = {"annotator", "args", "annotation"}
    answers = []
    seen = set()
    for index, declaration in enumerate(declarations, start=1):
        location = f"annotator_answer #{index}"
        if not isinstance(declaration, dict):
            raise ScenarioError(f"{name}: {location} must be a table")
        if set(declaration) != fields:
            raise ScenarioError(f"{name}: {location} takes exactly: {', '.join(sorted(fields))}")
        annotator = declaration["annotator"]
        if not isinstance(annotator, str) or not annotator:
            raise ScenarioError(f"{name}: {location}.annotator must be a non-empty string")
        args = declaration["args"]
        if not isinstance(args, dict) or not args:
            raise ScenarioError(f"{name}: {location}.args must be a non-empty table")
        annotation = declaration["annotation"]
        if not isinstance(annotation, dict) or set(annotation) != {"delta", "requires", "emits"}:
            raise ScenarioError(
                f"{name}: {location}.annotation must be a table with exactly: delta, requires, emits"
            )
        requires = annotation["requires"]
        if (
            not isinstance(requires, dict)
            or not isinstance(requires.get("history"), list)
            or not isinstance(requires.get("attention"), list)
        ):
            raise ScenarioError(
                f"{name}: {location}.annotation.requires must carry history and attention arrays"
            )
        if annotator not in declared:
            raise ScenarioError(f"{name}: {location}.annotator {annotator!r} is not declared by the scenario's policy")
        spec = declared[annotator]
        if set(args) != set(spec.inputs):
            raise ScenarioError(
                f"{name}: {location}.args keys {sorted(args)} cannot match a consult to {annotator!r}, "
                f"which maps {sorted(spec.inputs)}"
            )
        _within_mandate(name, location, annotation, spec)
        answer = AnnotatorAnswer(
            annotator=annotator,
            args=canonical_args(args),
            annotation=annotation,
        )
        if answer.request_key in seen:
            raise ScenarioError(f"{name}: duplicate answer for {answer.request_key!r}")
        seen.add(answer.request_key)
        answers.append(answer)
    return tuple(answers)


def _authority_answers_of(name: str, declarations: object) -> tuple[AuthorityAnswer, ...]:
    """Parse exact authority rulings served to one scenario's APPA episodes."""
    if not isinstance(declarations, list):
        raise ScenarioError(f"{name}: 'authority_answer' must be an array of tables")
    fields = {"authority", "tool", "ruling"}
    answers = []
    seen = set()
    for index, declaration in enumerate(declarations, start=1):
        location = f"authority_answer #{index}"
        if not isinstance(declaration, dict) or set(declaration) != fields:
            raise ScenarioError(f"{name}: {location} takes exactly: {', '.join(sorted(fields))}")
        for field_name in fields:
            value = declaration[field_name]
            if not isinstance(value, str) or not value:
                raise ScenarioError(f"{name}: {location}.{field_name} must be a non-empty string")
        if declaration["ruling"] not in ("approve", "deny"):
            raise ScenarioError(f"{name}: {location}.ruling must be 'approve' or 'deny'")
        key = (declaration["authority"], declaration["tool"])
        if key in seen:
            raise ScenarioError(f"{name}: duplicate authority answer for {key!r}")
        seen.add(key)
        answers.append(AuthorityAnswer(**declaration))
    return tuple(answers)


def _sanitizer_answers_of(name: str, declarations: object) -> tuple[SanitizerAnswer, ...]:
    """Parse deterministic sanitizer fixtures hosted for one scenario."""
    if not isinstance(declarations, list):
        raise ScenarioError(f"{name}: 'sanitizer_answer' must be an array of tables")
    fields = {"sanitizer", "drop_lines_containing"}
    answers = []
    seen = set()
    for index, declaration in enumerate(declarations, start=1):
        location = f"sanitizer_answer #{index}"
        if not isinstance(declaration, dict) or set(declaration) != fields:
            raise ScenarioError(f"{name}: {location} takes exactly: {', '.join(sorted(fields))}")
        sanitizer = declaration["sanitizer"]
        needles = declaration["drop_lines_containing"]
        if not isinstance(sanitizer, str) or not sanitizer:
            raise ScenarioError(f"{name}: {location}.sanitizer must be a non-empty string")
        if not isinstance(needles, list) or not needles or any(not isinstance(needle, str) or not needle for needle in needles):
            raise ScenarioError(f"{name}: {location}.drop_lines_containing must be a non-empty list of strings")
        if sanitizer in seen:
            raise ScenarioError(f"{name}: duplicate sanitizer answer for {sanitizer!r}")
        seen.add(sanitizer)
        answers.append(SanitizerAnswer(sanitizer=sanitizer, drop_lines_containing=tuple(needles)))
    return tuple(answers)


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

    raw_annotations = data.get("annotator_answer", [])
    authority_answers = _authority_answers_of(name, data.get("authority_answer", []))
    sanitizer_answers = _sanitizer_answers_of(name, data.get("sanitizer_answer", []))
    if (raw_annotations or authority_answers or sanitizer_answers) and policy_profile is None:
        raise ScenarioError(f"{name}: external fixture answers require a policy_profile")
    declared = _declared_annotators(name, policy_profile.appa) if policy_profile else {}
    annotator_answers = _annotator_answers_of(name, raw_annotations, declared)

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
        annotator_answers=annotator_answers,
        authority_answers=authority_answers,
        sanitizer_answers=sanitizer_answers,
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
            for needed in _systems_of_check(check):
                if needed not in systems:
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
