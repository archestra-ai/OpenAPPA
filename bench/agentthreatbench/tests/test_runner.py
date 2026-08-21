import hashlib
import json
from types import SimpleNamespace

import pytest
from inspect_ai.tool import ToolDef

from appa_agentthreatbench.fides import FIDES_BINDING_IDENTITY, FIDES_NATIVE_BINDING_IDENTITY
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
    FIDES_NATIVE_SCAFFOLD,
    FIDES_NATIVE_SECURITY_TOOLS,
    FIDES_SCAFFOLD,
    MEMORY_BRANCH_SCAFFOLD,
    MEMORY_CHILD_SCAFFOLD,
    SYSTEM_PROMPTS,
    ReturnComponentArguments,
    ReturnFieldArguments,
    _compile_return_schema,
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


def test_parent_field_description_compiles_without_answer_extraction() -> None:
    schema = _compile_return_schema(
        [
            ReturnFieldArguments(
                name="vacation_allowance",
                kind="integer",
                minimum=0,
                maximum=365,
                choices=[],
                unit="",
                components=[],
            )
        ]
    )
    assert schema == {
        "type": "object",
        "properties": {
            "vacation_allowance": {
                "type": "object",
                "properties": {
                    "known": {"type": "boolean"},
                    "value": {"type": "integer", "minimum": 0, "maximum": 365},
                },
                "required": ["known", "value"],
            }
        },
        "required": ["vacation_allowance"],
    }
    assert "20" not in json.dumps(schema)


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
        name="allowance",
        kind="measurement",
        minimum=0,
        maximum=365,
        choices=[],
        unit="days",
        components=[],
    )

    schema = _compile_return_schema([field])

    assert schema == {
        "type": "object",
        "properties": {
            "allowance": {
                "type": "object",
                "properties": {
                    "known": {"type": "boolean"},
                    "value": {"type": "integer", "minimum": 0, "maximum": 365},
                    "unit": {"type": "string", "enum": ["days"]},
                },
                "required": ["known", "value", "unit"],
            }
        },
        "required": ["allowance"],
    }
    assert "20" not in json.dumps(schema)
    assert _render_attested_facts(
        [field],
        '{"allowance":{"known":true,"unit":"days","value":20}}',
    ) == ["allowance=20 days (twenty days)"]
    assert (
        _render_attested_facts(
            [field],
            '{"allowance":{"known":false,"unit":"days","value":0}}',
        )
        == []
    )


def test_compound_compiles_and_renders_atomic_organizational_components() -> None:
    field = ReturnFieldArguments(
        name="request_endpoint",
        kind="compound",
        minimum=0,
        maximum=0,
        choices=[],
        unit="",
        components=[
            ReturnComponentArguments(name="owner", choices=["facilities", "procurement", "IT"]),
            ReturnComponentArguments(name="surface", choices=["portal", "email", "form"]),
        ],
    )

    schema = _compile_return_schema([field])

    assert schema["properties"] == {
        "request_endpoint": {
            "type": "object",
            "properties": {
                "known": {"type": "boolean"},
                "owner": {"type": "string", "enum": ["facilities", "procurement", "IT"]},
                "surface": {"type": "string", "enum": ["portal", "email", "form"]},
            },
            "required": ["known", "owner", "surface"],
        }
    }
    assert _render_attested_facts(
        [field],
        '{"request_endpoint":{"known":true,"owner":"facilities","surface":"portal"}}',
    ) == ["request_endpoint=facilities portal"]
    assert (
        _render_attested_facts(
            [field],
            '{"request_endpoint":{"known":false,"owner":"facilities","surface":"portal"}}',
        )
        == []
    )


def test_single_component_compound_preserves_a_generic_system_type() -> None:
    field = ReturnFieldArguments(
        name="booking_endpoint",
        kind="compound",
        minimum=0,
        maximum=0,
        choices=[],
        unit="",
        components=[ReturnComponentArguments(name="surface", choices=["calendar", "portal", "form"])],
    )

    _compile_return_schema([field])

    assert _render_attested_facts(
        [field],
        '{"booking_endpoint":{"known":true,"surface":"calendar"}}',
    ) == ["booking_endpoint=calendar"]


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
    original = {"recipient": "requesting_user", "body": "Employees receive paid vacation."}

    projected = _replace_with_attested_facts(original, ["allowance=20 days (twenty days)"])

    assert original["body"] == "Employees receive paid vacation."
    assert projected == {
        "recipient": "requesting_user",
        "body": "allowance=20 days (twenty days)",
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
