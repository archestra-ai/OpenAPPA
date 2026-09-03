"""The entrypoint's refusal set.

The stock kagent parser ignores unknown config fields; this entrypoint
does not. A field the runtime cannot gate must refuse the start — an
ungated feature running unseen is the one outcome the integration
exists to prevent. The refusals are named where a name helps the
operator, and generic where only completeness is at stake.
"""

from __future__ import annotations

from typing import Any

import pydantic
from pydantic import AliasChoices
from pydantic.fields import FieldInfo

from .plugin import RETURN_TOOL
from .wire import RESERVED_TOOL

# The keys the kagent `_McpTlsMixin` reads from the raw `params` dict of
# an MCP tool config, in its before-validator `_lift_tls_from_params`.
_LIFTED_TLS_KEYS = ("tls_insecure_skip_verify", "tls_ca_cert_path", "tls_disable_system_cas")

# Where a rendered kagent config names a tool: the tool filter of an MCP
# server, and the name of a remote agent, which the stock builder
# attaches as a tool of that name.
_MCP_TOOL_KEYS = ("http_tools", "sse_tools")
_REMOTE_AGENTS_KEY = "remote_agents"


class ConfigRefused(Exception):
    """The image refuses to start, and the message says why.

    The rendered config carries something this runtime cannot gate, or
    ``APPA_ENABLED`` carries a value outside its set. ``main`` prints
    the message and exits 2.
    """


def refuse_unsupported(config: dict[str, Any], model_cls: type[pydantic.BaseModel]) -> None:
    """Refuse a config with fields outside the validated schema.

    ``sub_agents`` gets its named refusal first: the python runtime has
    no in-process sub-agent field, so its presence means a Go-compiled
    config reached the python image — dropping the children silently
    would run a different agent than the operator declared. A declared
    tool named ``appa_return`` or ``execute_remedy_plan`` gets the next one
    (``_refuse_the_reserved_names``), and so does one named
    ``execute_remedy_plan``.

    The walk then reads the raw config beside the instance ``model_cls``
    validates it into. Each nested dict is checked against the class of
    the value pydantic built for it. For a discriminated union that
    class is the member pydantic chose, so a key of a sibling member is
    unknown. A key that pydantic reads through an ``AliasPath`` is
    unknown too (``_input_keys``). A config that fails validation raises
    ``pydantic.ValidationError`` from here.
    """
    if "sub_agents" in config:
        raise ConfigRefused(
            "the config carries in-process sub_agents, which the python runtime cannot represent — "
            "a Go-compiled config reached the python image; neither OpenAPPA image runs in-process "
            "sub-agents, so declare the children as remote agents"
        )
    _refuse_the_reserved_names(config)
    instance = model_cls.model_validate(config)
    unknown = sorted(_unknown_keys(config, instance, path=""))
    if unknown:
        raise ConfigRefused(
            "the config carries fields outside the runtime's schema, and the entrypoint does not "
            f"run what it cannot gate: {', '.join(unknown)}"
        )


# The tool names APPA owns: the return gate the plugin registers in a
# child scope, and the reserved tool the entrypoint appends over the
# runtime's own MCP endpoint. A config may declare neither.
_RESERVED_TOOL_NAMES = frozenset({RETURN_TOOL, RESERVED_TOOL})


def _refuse_the_reserved_names(config: dict[str, Any]) -> None:
    """Refuse a config that declares a tool APPA owns the name of.

    Two names are APPA's. The plugin registers its own return gate as
    ``appa_return`` on every model request of a child scope, and the
    entrypoint appends ``execute_remedy_plan`` over the runtime's own
    MCP endpoint after this guard runs. Each plugin recognizes what it
    owns by identity, so a declared tool of either name crosses the tool
    gate like any other tool. What such a tool cannot do is run as the
    operator meant: the model reads two declarations of one name, and
    which one answers is the builder's order, not the policy's. The
    operator reads the collision at startup instead.

    The walk reads the raw config: the tool filter of each MCP server,
    and the name of each remote agent, which the stock builder attaches
    as a tool of that name. A server that advertises the name under no
    filter is not declared here, and the plugin's identity check is what
    holds that case.
    """
    named: list[str] = []
    for key in _MCP_TOOL_KEYS:
        for index, server in _mappings(config.get(key)):
            filter_names = server.get("tools")
            if not isinstance(filter_names, list):
                continue
            named.extend(
                f"{key}.{index}.tools.{position}"
                for position, name in enumerate(filter_names)
                if name in _RESERVED_TOOL_NAMES
            )
    for index, remote in _mappings(config.get(_REMOTE_AGENTS_KEY)):
        if remote.get("name") in _RESERVED_TOOL_NAMES:
            named.append(f"{_REMOTE_AGENTS_KEY}.{index}.name")
    if named:
        reserved = ", ".join(sorted(_RESERVED_TOOL_NAMES))
        raise ConfigRefused(
            f"the config declares a tool under a name APPA owns ({reserved}) — rename the tool: {', '.join(named)}"
        )


def _mappings(raw: Any) -> list[tuple[int, dict[str, Any]]]:
    """The dict entries of a raw config list, with their positions.

    Every other shape declares no tool name, and the validation the
    caller runs next reaches it.
    """
    if not isinstance(raw, list):
        return []
    return [(index, entry) for index, entry in enumerate(raw) if isinstance(entry, dict)]


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


def _unknown_keys(raw: Any, validated: Any, path: str, extra_known: frozenset[str] = frozenset()) -> list[str]:
    """Walk the raw config beside its validated value and collect unknown key paths.

    A dict pairs with the model instance pydantic built from it. A list
    pairs with the validated list item by item. Every other pair ends the
    walk: scalars, ``None``, and plain-dict fields such as ``headers``,
    whose keys are data. ``extra_known`` names the raw keys of this dict
    that the parent instance reads itself. Their values are scalars, so
    the walk does not enter them.
    """
    if isinstance(raw, dict) and isinstance(validated, pydantic.BaseModel):
        # Class access: reading model_fields on an instance is deprecated.
        model_cls = type(validated)
        by_name = _validates_by_name(model_cls)
        known: dict[str, str] = {}
        for name, field in model_cls.model_fields.items():
            for key in _input_keys(name, field, by_name):
                known.setdefault(key, name)
        unknown: list[str] = []
        for key, nested in raw.items():
            if key in extra_known:
                continue
            name = known.get(key)
            if name is None:
                unknown.append(f"{path}{key}")
                continue
            child = getattr(validated, name)
            unknown.extend(_unknown_keys(nested, child, f"{path}{key}.", _lifted_keys(validated, key)))
        return unknown
    if isinstance(raw, list) and isinstance(validated, (list, tuple)):
        # Pydantic keeps the length of a list field. A mismatch breaks
        # that invariant and stops the start.
        unknown = []
        for index, (item, built) in enumerate(zip(raw, validated, strict=True)):
            unknown.extend(_unknown_keys(item, built, f"{path}{index}."))
        return unknown
    return []


def _lifted_keys(validated: pydantic.BaseModel, key: str) -> frozenset[str]:
    """The keys of the raw ``params`` dict that a kagent MCP tool config reads itself.

    The kagent ``_McpTlsMixin`` copies the three TLS keys from ``params`` to
    its own fields before validation. The ADK connection params declare
    none of them. A parent that declares the field reads the key.
    """
    if key != "params":
        return frozenset()
    return frozenset(name for name in _LIFTED_TLS_KEYS if name in type(validated).model_fields)


def _validates_by_name(model_cls: type[pydantic.BaseModel]) -> bool:
    """Whether pydantic reads an aliased field of this class from its bare name too."""
    config = model_cls.model_config
    return bool(config.get("validate_by_name") or config.get("populate_by_name"))


def _input_keys(name: str, field: FieldInfo, by_name: bool) -> set[str]:
    """The raw keys pydantic reads this field from.

    The bare name counts when the field has no alias, or when the class
    validates by name. Every string alias counts, alone or as a choice.
    An ``AliasPath`` contributes no key, alone or as a choice: pydantic
    reads it from inside a nested dict, and the walk cannot pair that
    dict with a validated model. So a key read that way is unknown, and
    the guard refuses the config. kagent-adk v0.9.12 declares no
    ``AliasPath``.
    """
    keys: set[str] = set()
    if by_name or (field.alias is None and field.validation_alias is None):
        keys.add(name)
    if field.alias is not None:
        keys.add(field.alias)
    validation_alias = field.validation_alias
    if isinstance(validation_alias, str):
        keys.add(validation_alias)
    elif isinstance(validation_alias, AliasChoices):
        keys.update(choice for choice in validation_alias.choices if isinstance(choice, str))
    return keys
