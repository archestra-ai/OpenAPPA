"""Strict, immutable FIDES profile configuration."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from types import MappingProxyType
from typing import Any, Mapping, TypeVar

from agent_framework.security import ConfidentialityLabel, IntegrityLabel

from .systems import System

PROFILE_VERSION = 1
LABELED_SYSTEMS = (
    System.HR,
    System.FINANCE,
    System.TASK_TRACKER,
    System.PUBLIC_FORUM,
    System.VENDOR,
)
ALL_TOOL_NAMES: frozenset[str] = frozenset(
    f"{verb}_{system.dir_name}" for system in LABELED_SYSTEMS for verb in ("search", "read", "create")
) | {"send_email", "share_legal_packet"}


class ProfileError(ValueError):
    """A profile is not valid version 1 configuration."""


@dataclass(frozen=True)
class ResultLabel:
    integrity: IntegrityLabel
    confidentiality: ConfidentialityLabel


@dataclass(frozen=True)
class ToolPolicy:
    accepts_untrusted: bool
    max_allowed_confidentiality: ConfidentialityLabel | None = None


@dataclass(frozen=True)
class Profile:
    version: int
    systems: Mapping[System, ResultLabel]
    tools: Mapping[str, ToolPolicy]

    def __post_init__(self) -> None:
        object.__setattr__(self, "systems", MappingProxyType(dict(self.systems)))
        object.__setattr__(self, "tools", MappingProxyType(dict(self.tools)))


def _defaults() -> Profile:
    systems = {
        System.HR: ResultLabel(IntegrityLabel.TRUSTED, ConfidentialityLabel.PRIVATE),
        System.FINANCE: ResultLabel(IntegrityLabel.TRUSTED, ConfidentialityLabel.PRIVATE),
        System.TASK_TRACKER: ResultLabel(IntegrityLabel.TRUSTED, ConfidentialityLabel.PUBLIC),
        System.PUBLIC_FORUM: ResultLabel(IntegrityLabel.UNTRUSTED, ConfidentialityLabel.PUBLIC),
        System.VENDOR: ResultLabel(IntegrityLabel.TRUSTED, ConfidentialityLabel.PUBLIC),
    }
    tools = {
        f"{verb}_{system.dir_name}": ToolPolicy(accepts_untrusted=True)
        for system in LABELED_SYSTEMS
        for verb in ("search", "read", "create")
    }
    tools["create_task_tracker"] = ToolPolicy(accepts_untrusted=False)
    tools["create_public_forum"] = ToolPolicy(True, ConfidentialityLabel.PUBLIC)
    tools["send_email"] = ToolPolicy(False, ConfidentialityLabel.PUBLIC)
    tools["share_legal_packet"] = ToolPolicy(False, ConfidentialityLabel.PUBLIC)
    return Profile(
        version=PROFILE_VERSION,
        systems=systems,
        tools=tools,
    )


DEFAULT_PROFILE = _defaults()


def _object(value: Any, location: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ProfileError(f"{location} must be a JSON object")
    return value


def _check_fields(value: Mapping[str, Any], allowed: set[str], location: str) -> None:
    unknown = value.keys() - allowed
    if unknown:
        raise ProfileError(f"unknown field {location}.{sorted(unknown)[0]}")


_Label = TypeVar("_Label", IntegrityLabel, ConfidentialityLabel)


def _enum(enum_type: type[_Label], value: Any, location: str) -> _Label:
    try:
        return enum_type(value)
    except (TypeError, ValueError) as exc:
        raise ProfileError(f"invalid value for {location}: {value!r}") from exc


def _pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ProfileError(f"duplicate field {key!r}")
        result[key] = value
    return result


def _constant(value: str) -> None:
    raise ProfileError(f"invalid JSON constant {value}")


def load_profile(path: str | Path) -> Profile:
    """Load a version 1 profile and apply its overrides to immutable defaults."""
    profile_path = Path(path)
    try:
        raw = json.loads(
            profile_path.read_text(encoding="utf-8"),
            object_pairs_hook=_pairs,
            parse_constant=_constant,
        )
    except OSError as exc:
        raise ProfileError(f"cannot read profile {profile_path}: {exc}") from exc
    except UnicodeError as exc:
        raise ProfileError(f"profile {profile_path} is not UTF-8") from exc
    except json.JSONDecodeError as exc:
        raise ProfileError(f"invalid JSON in profile {profile_path}: {exc.msg}") from exc

    root = _object(raw, "profile")
    _check_fields(root, {"version", "systems", "tools"}, "profile")
    version = root.get("version")
    if type(version) is not int or version != PROFILE_VERSION:
        raise ProfileError(f"profile.version must be {PROFILE_VERSION}")

    systems = dict(DEFAULT_PROFILE.systems)
    for name, raw_label in _object(root.get("systems", {}), "profile.systems").items():
        try:
            system = System(name)
        except ValueError as exc:
            raise ProfileError(f"unknown system {name!r}") from exc
        if system not in systems:
            raise ProfileError(f"system {name!r} has no result label")
        label = _object(raw_label, f"profile.systems.{name}")
        _check_fields(label, {"integrity", "confidentiality"}, f"profile.systems.{name}")
        if set(label) != {"integrity", "confidentiality"}:
            raise ProfileError(f"profile.systems.{name} requires integrity and confidentiality")
        systems[system] = ResultLabel(
            _enum(IntegrityLabel, label["integrity"], f"profile.systems.{name}.integrity"),
            _enum(ConfidentialityLabel, label["confidentiality"], f"profile.systems.{name}.confidentiality"),
        )

    tools = dict(DEFAULT_PROFILE.tools)
    for name, raw_policy in _object(root.get("tools", {}), "profile.tools").items():
        if name not in tools:
            raise ProfileError(f"unknown tool {name!r}")
        policy = _object(raw_policy, f"profile.tools.{name}")
        _check_fields(
            policy,
            {"accepts_untrusted", "max_allowed_confidentiality"},
            f"profile.tools.{name}",
        )
        current = tools[name]
        accepts_untrusted = policy.get("accepts_untrusted", current.accepts_untrusted)
        if type(accepts_untrusted) is not bool:
            raise ProfileError(f"profile.tools.{name}.accepts_untrusted must be a boolean")
        max_confidentiality = current.max_allowed_confidentiality
        if "max_allowed_confidentiality" in policy:
            max_confidentiality = _enum(
                ConfidentialityLabel,
                policy["max_allowed_confidentiality"],
                f"profile.tools.{name}.max_allowed_confidentiality",
            )
        tools[name] = ToolPolicy(accepts_untrusted, max_confidentiality)

    return Profile(PROFILE_VERSION, systems, tools)
