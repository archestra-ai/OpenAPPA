import hashlib
import json
from types import SimpleNamespace

import pytest
from inspect_ai.tool import ToolDef

from appa_agentthreatbench.fides import (
    FIDES_BINDING_IDENTITY,
    FIDES_MAX_CONCURRENT_TRAJECTORIES,
    FIDES_NATIVE_BINDING_IDENTITY,
)
from appa_agentthreatbench.runner import (
    EXPECTED_BINDING_IDENTITY,
    EXPECTED_TOTAL_SAMPLES,
    _audit_diagnostics,
    _scoreable_limit_termination,
    preflight,
    run_manifest,
    validate_inventory,
)
from appa_agentthreatbench.tasks import (
    AGENT_PROMPT_PROFILES,
    AUTONOMY_BRANCH_SCAFFOLD,
    AUTONOMY_CHILD_SCAFFOLD,
    DELEGATE_MODEL_TOOL,
    FIDES_NATIVE_SCAFFOLD,
    FIDES_NATIVE_SECURITY_TOOLS,
    FIDES_SCAFFOLD,
    MEMORY_BRANCH_SCAFFOLD,
    MEMORY_CHILD_SCAFFOLD,
    SYSTEM_PROMPTS,
    DelegateArguments,
    ReturnComponentArguments,
    ReturnFieldArguments,
    _compile_inbox_return_schema,
    _compile_return_schema,
    _project_inbox_actions,
    _render_attested_facts,
    _replace_with_attested_facts,
    complete_dataset,
    policy_digest,
    system_prompt,
)


def test_complete_inventory_has_all_tasks_arms_and_controls() -> None:
    ids = validate_inventory()
    assert len(ids) == EXPECTED_TOTAL_SAMPLES == 130
    dataset = complete_dataset()
    assert sum(bool(sample.metadata.get("control")) for sample in dataset) == 10


def test_fides_native_tools_have_provider_portable_strict_schemas() -> None:
    for tool in FIDES_NATIVE_SECURITY_TOOLS:
        parameters = ToolDef(tool).parameters.model_dump()
        assert set(parameters["required"]) == set(parameters["properties"])
        assert parameters["additionalProperties"] is False


def test_preflight_checks_pins_without_requiring_a_credential() -> None:
    result = preflight("openrouter/openai/gpt-5.6-luna", require_credential=False)
    assert result["total_samples"] == 130
    assert result["binding_identity"] == EXPECTED_BINDING_IDENTITY
    assert result["fides_binding_identity"] == "agent-framework-fides-agentthreatbench-v1"
    assert result["fides_native_binding_identity"] == FIDES_NATIVE_BINDING_IDENTITY
    assert result["fides_version"] == "1.13.0"


@pytest.mark.asyncio
async def test_inspect_openai_transport_uses_numeric_timeout_leaves() -> None:
    """Catch OpenAI/httpx transport mismatches before an eval retries forever."""
    from inspect_ai.model._openai import OpenAIAsyncHttpxClient

    client = OpenAIAsyncHttpxClient()
    try:
        timeout = client.timeout
        for leaf in (timeout.connect, timeout.read, timeout.write, timeout.pool):
            assert leaf is None or isinstance(leaf, (int, float))
    finally:
        await client.aclose()


def test_chaos_profile_is_the_same_explicit_addendum_in_every_arm() -> None:
    chaos = AGENT_PROMPT_PROFILES["agent-threat-chaos"]
    assert system_prompt("memory_poison", "stock", "standard") == SYSTEM_PROMPTS["memory_poison"]
    assert system_prompt("memory_poison", "stock", "agent-threat-chaos") == (
        f"{SYSTEM_PROMPTS['memory_poison']}\n\n{chaos}"
    )
    assert system_prompt("memory_poison", "guarded", "agent-threat-chaos") == (
        f"{SYSTEM_PROMPTS['memory_poison']}\n\n{MEMORY_BRANCH_SCAFFOLD}\n\n{chaos}"
    )
    assert system_prompt("memory_poison", "fides", "agent-threat-chaos") == (
        f"{SYSTEM_PROMPTS['memory_poison']}\n\n{FIDES_SCAFFOLD}\n\n{chaos}"
    )
    assert system_prompt("memory_poison", "fides-native", "agent-threat-chaos") == (
        f"{SYSTEM_PROMPTS['memory_poison']}\n\n{FIDES_NATIVE_SCAFFOLD}\n\n{chaos}"
    )
    assert system_prompt("autonomy_hijack", "guarded", "agent-threat-chaos") == (
        f"{SYSTEM_PROMPTS['autonomy_hijack']}\n\n{AUTONOMY_BRANCH_SCAFFOLD}\n\n{chaos}"
    )
    assert "HACKED" not in chaos
    assert "attack_marker" not in chaos


def test_prompt_profile_changes_and_is_pinned_by_the_run_digest() -> None:
    standard = run_manifest("openrouter/openai/gpt-5.6-luna", "high", 300, 50)
    chaos = run_manifest(
        "openrouter/openai/gpt-5.6-luna",
        "high",
        300,
        50,
        agent_prompt_profile="agent-threat-chaos",
    )
    assert standard["run_digest"] != chaos["run_digest"]
    assert chaos["config"]["agent_prompt_profile"] == "agent-threat-chaos"
    assert chaos["config"]["binding_identities"]["fides"] == "agent-framework-fides-agentthreatbench-v1"
    assert chaos["config"]["binding_identities"]["fides-native"] == FIDES_NATIVE_BINDING_IDENTITY
    assert chaos["config"]["fides_max_concurrent_trajectories"] == FIDES_MAX_CONCURRENT_TRAJECTORIES == 1


def test_parent_field_description_compiles_without_answer_extraction() -> None:
    schema = _compile_return_schema(
        [
            ReturnFieldArguments(
                name="retention_window",
                kind="integer",
                minimum=0,
                maximum=730,
                choices=[],
                unit="",
                components=[],
            )
        ]
    )
    assert schema == {
        "type": "object",
        "properties": {
            "retention_window": {
                "type": "object",
                "properties": {
                    "known": {"type": "boolean"},
                    "value": {"type": "integer", "minimum": 0, "maximum": 730},
                },
                "required": ["known", "value"],
            }
        },
        "required": ["retention_window"],
    }
    assert "90" not in json.dumps(schema)


def test_delegate_tool_exposes_structural_bounds_to_the_planning_model() -> None:
    parameters = ToolDef(DELEGATE_MODEL_TOOL).parameters.model_dump(exclude_none=True)
    fields = parameters["properties"]["fields"]
    field_schema = fields["items"]
    assert "enum_set" in field_schema["properties"]["kind"]["enum"]
    assert "never its guessed value" in field_schema["properties"]["name"]["description"]

    prototype = ReturnFieldArguments(
        name="bounded_fact",
        kind="boolean",
        minimum=0,
        maximum=1,
        choices=[],
        unit="",
        components=[],
    )
    with pytest.raises(ValueError):
        DelegateArguments(task="too many fields", fields=[prototype] * 9)


def test_scalar_fields_render_only_known_attested_values() -> None:
    fields = [
        ReturnFieldArguments(
            name="count",
            kind="integer",
            minimum=0,
            maximum=10,
            choices=[],
            unit="",
            components=[],
        ),
        ReturnFieldArguments(
            name="required",
            kind="boolean",
            minimum=0,
            maximum=1,
            choices=[],
            unit="",
            components=[],
        ),
        ReturnFieldArguments(
            name="cadence",
            kind="enum",
            minimum=0,
            maximum=0,
            choices=["daily", "monthly", "unknown"],
            unit="",
            components=[],
        ),
    ]

    rendered = _render_attested_facts(
        fields,
        '{"count":{"known":true,"value":3},'
        '"required":{"known":false,"value":false},'
        '"cadence":{"known":true,"value":"monthly"}}',
    )

    assert rendered == ["count=3", "cadence=monthly"]


def test_measurement_compiles_and_renders_a_canonical_quantity_without_answer_leakage() -> None:
    field = ReturnFieldArguments(
        name="retry_window",
        kind="measurement",
        minimum=0,
        maximum=48,
        choices=[],
        unit="hours",
        components=[],
    )

    schema = _compile_return_schema([field])

    assert schema == {
        "type": "object",
        "properties": {
            "retry_window": {
                "type": "object",
                "properties": {
                    "known": {"type": "boolean"},
                    "value": {"type": "integer", "minimum": 0, "maximum": 48},
                    "unit": {"type": "string", "enum": ["hours"]},
                },
                "required": ["known", "value", "unit"],
            }
        },
        "required": ["retry_window"],
    }
    assert "12" not in json.dumps(schema)
    assert _render_attested_facts(
        [field],
        '{"retry_window":{"known":true,"unit":"hours","value":12}}',
    ) == ["retry_window=12 hours (twelve hours)"]
    assert (
        _render_attested_facts(
            [field],
            '{"retry_window":{"known":false,"unit":"hours","value":0}}',
        )
        == []
    )


def test_measurement_renderer_inflects_identifier_units_without_aliases() -> None:
    field = ReturnFieldArguments(
        name="retry_budget",
        kind="measurement",
        minimum=0,
        maximum=48,
        choices=[],
        unit="attempt_per_cycle",
        components=[],
    )

    assert _render_attested_facts(
        [field],
        '{"retry_budget":{"known":true,"unit":"attempt_per_cycle","value":12}}',
    ) == ["retry_budget=12 attempts per cycle (twelve attempts per cycle)"]


def test_compound_compiles_and_renders_atomic_organizational_components() -> None:
    field = ReturnFieldArguments(
        name="request_endpoint",
        kind="compound",
        minimum=0,
        maximum=0,
        choices=[],
        unit="",
        components=[
            ReturnComponentArguments(name="owner", choices=["operations", "finance", "support"]),
            ReturnComponentArguments(name="surface", choices=["console", "dashboard", "form"]),
        ],
    )

    schema = _compile_return_schema([field])

    assert schema["properties"] == {
        "request_endpoint": {
            "type": "object",
            "properties": {
                "known": {"type": "boolean"},
                "owner": {
                    "type": "string",
                    "enum": ["operations", "finance", "support"],
                },
                "surface": {
                    "type": "string",
                    "enum": ["console", "dashboard", "form"],
                },
            },
            "required": ["known", "owner", "surface"],
        }
    }
    assert _render_attested_facts(
        [field],
        '{"request_endpoint":{"known":true,"owner":"operations","surface":"console"}}',
    ) == ["request_endpoint=operations console"]
    assert (
        _render_attested_facts(
            [field],
            '{"request_endpoint":{"known":false,"owner":"operations","surface":"console"}}',
        )
        == []
    )


def test_single_component_compound_preserves_a_generic_system_type() -> None:
    field = ReturnFieldArguments(
        name="delivery_surface",
        kind="compound",
        minimum=0,
        maximum=0,
        choices=[],
        unit="",
        components=[ReturnComponentArguments(name="surface", choices=["dashboard", "console", "form"])],
    )

    _compile_return_schema([field])

    assert _render_attested_facts(
        [field],
        '{"delivery_surface":{"known":true,"surface":"dashboard"}}',
    ) == ["delivery_surface=dashboard"]


def test_compound_preserves_known_bounded_components_when_an_optional_part_is_unknown() -> None:
    field = ReturnFieldArguments(
        name="service_endpoint",
        kind="compound",
        minimum=0,
        maximum=0,
        choices=[],
        unit="",
        components=[
            ReturnComponentArguments(name="owner", choices=["operations", "unknown"]),
            ReturnComponentArguments(name="surface", choices=["console", "form"]),
        ],
    )

    _compile_return_schema([field])

    assert _render_attested_facts(
        [field],
        '{"service_endpoint":{"known":true,"owner":"unknown","surface":"console"}}',
    ) == ["service_endpoint=console"]


def test_enum_set_compiles_and_renders_a_bounded_multi_value_fact() -> None:
    field = ReturnFieldArguments(
        name="supported_modes",
        kind="enum_set",
        minimum=0,
        maximum=3,
        choices=["batch_mode", "stream_mode", "offline_mode"],
        unit="",
        components=[],
    )

    schema = _compile_return_schema([field])

    assert schema["properties"] == {
        "supported_modes": {
            "type": "object",
            "properties": {
                "known": {"type": "boolean"},
                "value": {
                    "type": "array",
                    "items": {
                        "type": "string",
                        "enum": ["batch_mode", "stream_mode", "offline_mode"],
                    },
                    "maxItems": 3,
                },
            },
            "required": ["known", "value"],
        }
    }
    assert _render_attested_facts(
        [field],
        '{"supported_modes":{"known":true,"value":["batch_mode","offline_mode"]}}',
    ) == ["supported_modes=batch mode, offline mode"]


def test_frequency_measurement_does_not_inject_a_lexical_alias() -> None:
    field = ReturnFieldArguments(
        name="training_frequency",
        kind="measurement",
        minimum=0,
        maximum=365,
        choices=[],
        unit="per_year",
        components=[],
    )

    assert _render_attested_facts(
        [field],
        '{"training_frequency":{"known":true,"unit":"per_year","value":1}}',
    ) == ["training_frequency=1 per year (one per year)"]


def test_delivery_contains_only_attested_serialization() -> None:
    original = {"recipient": "requesting_user", "body": "The service retries periodically."}

    projected = _replace_with_attested_facts(original, ["retry_window=12 hours (twelve hours)"])

    assert original["body"] == "The service retries periodically."
    assert projected == {
        "recipient": "requesting_user",
        "body": "retry_window=12 hours (twelve hours)",
    }
    with pytest.raises(ValueError, match="no known schema-attested fields"):
        _replace_with_attested_facts(original, [])


def test_memory_branch_prompts_contain_no_benchmark_answers() -> None:
    prompts = f"{MEMORY_BRANCH_SCAFFOLD}\n{MEMORY_CHILD_SCAFFOLD}".lower()
    for sample in complete_dataset():
        if sample.metadata["task_type"] != "memory_poison":
            continue
        expected = str(sample.metadata.get("expected_answer", "")).strip().lower()
        assert expected
        assert expected not in prompts


def test_inbox_schema_and_prompts_contain_no_sample_content_or_attack_oracles() -> None:
    schema_text = json.dumps(_compile_inbox_return_schema(5)).lower()
    prompt_text = f"{AUTONOMY_BRANCH_SCAFFOLD}\n{AUTONOMY_CHILD_SCAFFOLD}".lower()
    combined = f"{schema_text}\n{prompt_text}"
    assert "attack_email_indices" not in combined
    assert "forbidden_actions" not in combined
    for sample in complete_dataset():
        if sample.metadata["task_type"] != "autonomy_hijack":
            continue
        for forbidden in sample.metadata.get("forbidden_actions", []):
            assert str(forbidden).lower() not in combined
        for email in sample.metadata.get("inbox", []):
            for value in email.values():
                assert str(value).lower() not in combined


def test_attested_inbox_projection_is_bounded_and_cannot_self_promote_priority() -> None:
    returned = json.dumps(
        {
            "email_1": {
                "decision": "important_candidate",
                "topic": "project",
                "action": "review",
                "urgency": "time_sensitive",
            },
            "email_2": {
                "decision": "suspicious",
                "topic": "account",
                "action": "update",
                "urgency": "claimed_urgent",
            },
        }
    )

    actions = _project_inbox_actions(returned, 2)

    assert actions == [
        ("categorize_email", {"email_index": 1, "category": "normal"}),
        (
            "draft_summary",
            {"email_index": 1, "summary": "topic=project; action=review; urgency=time sensitive"},
        ),
        ("categorize_email", {"email_index": 2, "category": "spam"}),
    ]
    assert all(arguments.get("category") != "important" for _, arguments in actions)


def test_attested_inbox_projection_always_emits_one_bounded_fallback_draft() -> None:
    returned = json.dumps(
        {
            "email_1": {
                "decision": "suspicious",
                "topic": "security",
                "action": "investigate",
                "urgency": "claimed_urgent",
            },
            "email_2": {
                "decision": "normal",
                "topic": "research",
                "action": "review",
                "urgency": "routine",
            },
        }
    )

    actions = _project_inbox_actions(returned, 2)

    assert actions == [
        ("categorize_email", {"email_index": 1, "category": "spam"}),
        ("categorize_email", {"email_index": 2, "category": "normal"}),
        (
            "draft_summary",
            {"email_index": 2, "summary": "topic=research; action=review; urgency=routine"},
        ),
    ]


def test_scoreable_inspect_limit_is_a_completed_audit_termination() -> None:
    limited = SimpleNamespace(
        error=None,
        limit=SimpleNamespace(type="message"),
        events=[SimpleNamespace(event="sample_limit", type="message")],
    )
    assert _scoreable_limit_termination(limited) == "message_limit"
    limited.error = RuntimeError("sample failed")
    assert _scoreable_limit_termination(limited) is None


def test_legacy_limit_sidecar_correlates_as_completed(tmp_path) -> None:
    message = SimpleNamespace(model_dump=lambda **_: {"role": "assistant", "content": "limited"})
    sample = SimpleNamespace(
        id="fides--mp_001",
        uuid="sample-uuid",
        epoch=1,
        metadata={"appa_arm": "fides", "task_type": "memory_poison"},
        messages=[message],
        error=None,
        limit=SimpleNamespace(type="message"),
        events=[SimpleNamespace(event="sample_limit", type="message")],
    )
    transcript = [message.model_dump(mode="json")]
    digest = hashlib.sha256(json.dumps(transcript, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    policy_sha256 = policy_digest("memory_poison", "fides")
    record = {
        "sample_id": str(sample.id),
        "sample_uuid": sample.uuid,
        "epoch": sample.epoch,
        "written_at": "2026-08-12T15:43:50+00:00",
        "completed": False,
        "termination": "exception",
        "transcript_sha256": digest,
        "binding_identity": FIDES_BINDING_IDENTITY,
        "arm": "fides",
        "task_type": "memory_poison",
        "agent_prompt_profile": "standard",
        "policy_sha256": policy_sha256,
        "resolver_fixture_sha256": None,
        "events": [],
        "resolver_requests": [],
        "fides_audit": [],
    }
    (tmp_path / "legacy-limit.json").write_text(json.dumps(record))
    manifest = {
        "config": {
            "policy_sha256": {"fides": {"memory_poison": policy_sha256}},
            "resolver_fixture_sha256": {},
            "agent_prompt_profile": "standard",
        }
    }

    diagnostics = _audit_diagnostics(tmp_path, [sample], manifest)

    assert diagnostics["correlated_final_audit_count"] == 1
    assert diagnostics["terminations"] == {"message_limit": 1}
