"""The entrypoint's refusal set.

The stock kagent parser ignores unknown config fields; this entrypoint
does not. A field the runtime cannot gate must refuse the start — an
ungated feature running unseen is the one outcome the integration
exists to prevent. The refusals are named where a name helps the
operator, and generic where only completeness is at stake.
"""

from __future__ import annotations

import types
import typing
from typing import Any

import pydantic

_UNION_TYPE = types.UnionType


class ConfigRefused(Exception):
    """The rendered config carries something this runtime cannot gate."""


def refuse_unsupported(config: dict[str, Any], model_cls: type[pydantic.BaseModel]) -> None:
    """Refuse a config with fields outside the validated schema.

    ``sub_agents`` gets its named refusal first: the python runtime has
    no in-process sub-agent field, so its presence means a Go-compiled
    config reached the python image — dropping the children silently
    would run a different agent than the operator declared.
    """
    if "sub_agents" in config:
        raise ConfigRefused(
            "the config carries in-process sub_agents, which the python runtime cannot represent — "
            "a Go-compiled config reached the python image; run it on appa-kagent-adk-go, or "
            "recompile the agent for the python runtime"
        )
    unknown = sorted(_unknown_keys(config, model_cls, path=""))
    if unknown:
        raise ConfigRefused(
            "the config carries fields outside the runtime's schema, and the entrypoint does not "
            f"run what it cannot gate: {', '.join(unknown)}"
        )


def refuse_divergent_summarizer(agent_config: Any) -> None:
    """Constrain the compaction summarizer to the agent model.

    A summarizer that names a different model opens a model egress no
    hook gates. The same model adds no egress the agent lacks.
    """
    context = getattr(agent_config, "context_config", None)
    compaction = getattr(context, "compaction", None) if context else None
    summarizer = getattr(compaction, "summarizer_model", None) if compaction else None
    if summarizer is None:
        return
    if summarizer.model_dump(exclude_none=True) != agent_config.model.model_dump(exclude_none=True):
        raise ConfigRefused(
            "the compaction summarizer names a model other than the agent model, an egress no hook "
            "gates — drop context_config.compaction.summarizer_model or set it to the agent model"
        )


def _unknown_keys(value: Any, annotation: Any, path: str) -> list[str]:
    """Walk the config against the schema and collect unknown keys."""
    annotation = _unwrap(annotation)
    if isinstance(value, dict):
        models = _model_members(annotation)
        if not models:
            return []
        unknown: list[str] = []
        known: set[str] = set()
        for model in models:
            for name, field in model.model_fields.items():
                known.add(field.alias or name)
                known.add(name)
        for key, nested in value.items():
            if key not in known:
                unknown.append(f"{path}{key}")
                continue
            for model in models:
                field = _field_for(model, key)
                if field is not None:
                    unknown.extend(_unknown_keys(nested, field.annotation, f"{path}{key}."))
                    break
        return unknown
    if isinstance(value, list):
        item_annotation = _item_annotation(annotation)
        unknown = []
        for index, item in enumerate(value):
            unknown.extend(_unknown_keys(item, item_annotation, f"{path}{index}."))
        return unknown
    return []


def _unwrap(annotation: Any) -> Any:
    origin = typing.get_origin(annotation)
    if origin is None:
        return annotation
    return annotation


def _model_members(annotation: Any) -> list[type[pydantic.BaseModel]]:
    """The pydantic models an annotation can validate a dict into."""
    if isinstance(annotation, type) and issubclass(annotation, pydantic.BaseModel):
        return [annotation]
    origin = typing.get_origin(annotation)
    if origin is typing.Annotated:
        return _model_members(typing.get_args(annotation)[0])
    if origin in (typing.Union, _UNION_TYPE):
        members: list[type[pydantic.BaseModel]] = []
        for arg in typing.get_args(annotation):
            members.extend(_model_members(arg))
        return members
    return []


def _item_annotation(annotation: Any) -> Any:
    origin = typing.get_origin(annotation)
    if origin is typing.Annotated:
        return _item_annotation(typing.get_args(annotation)[0])
    if origin in (typing.Union, _UNION_TYPE):
        for arg in typing.get_args(annotation):
            item = _item_annotation(arg)
            if item is not None:
                return item
        return None
    if origin in (list, tuple, set):
        args = typing.get_args(annotation)
        return args[0] if args else None
    return None


def _field_for(model: type[pydantic.BaseModel], key: str):
    for name, field in model.model_fields.items():
        if key in (name, field.alias):
            return field
    return None
