import hashlib
import json
from types import SimpleNamespace

from appa_agentthreatbench.fides import FIDES_BINDING_IDENTITY
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
    FIDES_SCAFFOLD,
    MEMORY_BRANCH_SCAFFOLD,
    MEMORY_CHILD_SCAFFOLD,
    SYSTEM_PROMPTS,
    ReturnFieldArguments,
    _compile_return_schema,
    complete_dataset,
    policy_digest,
    system_prompt,
)


def test_complete_inventory_has_all_tasks_arms_and_controls() -> None:
    ids = validate_inventory()
    assert len(ids) == EXPECTED_TOTAL_SAMPLES == 104
    dataset = complete_dataset()
    assert sum(bool(sample.metadata.get("control")) for sample in dataset) == 8


def test_preflight_checks_pins_without_requiring_a_credential() -> None:
    result = preflight("openrouter/openai/gpt-5.6-luna", require_credential=False)
    assert result["total_samples"] == 104
    assert result["binding_identity"] == EXPECTED_BINDING_IDENTITY
    assert result["fides_binding_identity"] == "agent-framework-fides-agentthreatbench-v1"
    assert result["fides_version"] == "1.13.0"


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


def test_parent_field_description_compiles_without_answer_extraction() -> None:
    schema = _compile_return_schema(
        [
            ReturnFieldArguments(
                name="vacation_days",
                kind="integer",
                minimum=0,
                maximum=365,
                choices=[],
            )
        ]
    )
    assert schema == {
        "type": "object",
        "properties": {
            "vacation_days": {
                "type": "integer",
                "minimum": 0,
                "maximum": 365,
            }
        },
        "required": ["vacation_days"],
    }
    assert "20" not in json.dumps(schema)


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
