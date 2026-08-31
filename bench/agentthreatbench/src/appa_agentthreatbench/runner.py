"""Preflight, execute, and summarize the complete evaluation."""

import hashlib
import importlib.metadata
import json
import os
import re
from collections import Counter, defaultdict
from datetime import UTC, datetime
from pathlib import Path

from inspect_ai import eval_set
from inspect_ai.log import EvalLog, EvalSample, read_eval_log
from inspect_ai.scorer import CORRECT
from inspect_ai.tool import ToolDef

from appa_agentthreatbench import INSPECT_AI_VERSION, INSPECT_EVALS_REVISION, UPSTREAM_SAMPLE_COUNTS
from appa_agentthreatbench.annotator import AnnotatorFixture, annotator_fixture_digest, mandate_readers
from appa_agentthreatbench.fides import (
    FIDES_BINDING_IDENTITY,
    FIDES_MAX_CONCURRENT_TRAJECTORIES,
    FIDES_NATIVE_BINDING_IDENTITY,
    FIDES_VERSION,
    FidesSession,
    fides_policy,
)
from appa_agentthreatbench.native import BINDING_IDENTITY, NativeSession
from appa_agentthreatbench.tasks import (
    AGENT_PROMPT_PROFILES,
    ARMS,
    DELEGATE_INBOX_TOOL,
    DELEGATE_TOOL,
    DOMAIN_TOOLS,
    FIDES_NATIVE_SECURITY_TOOLS,
    RESPONSE_TOOL,
    TASK_TYPES,
    complete_dataset,
    complete_task,
    guarded_externals,
    guarded_policy,
    permissive_policy,
    policy_digest,
)

PACKAGE_ROOT = Path(__file__).resolve().parent
EXPECTED_BINDING_IDENTITY = "appa-agent-python-v6"
EXPECTED_FIDES_VERSION = "1.13.0"
EXPECTED_UPSTREAM_SAMPLES = sum(UPSTREAM_SAMPLE_COUNTS.values())
EXPECTED_TOTAL_SAMPLES = (EXPECTED_UPSTREAM_SAMPLES + 2) * len(ARMS)
SMOKE_SAMPLE_IDS = (
    "stock--ah_005",
    "permissive--ah_005",
    "guarded--ah_005",
    "fides--ah_005",
    "fides-native--ah_005",
    "stock--de_001",
    "permissive--de_001",
    "guarded--de_001",
    "fides--de_001",
    "fides-native--de_001",
    "permissive--de_control_authorized",
    "guarded--de_control_authorized",
    "fides--de_control_authorized",
    "fides-native--de_control_authorized",
    "guarded--de_control_unauthorized",
    "fides--de_control_unauthorized",
    "fides-native--de_control_unauthorized",
    "guarded--mp_001",
    "fides--mp_001",
    "fides-native--mp_001",
)


def slug(value: str) -> str:
    return re.sub(r"[^a-zA-Z0-9_.-]+", "_", value)


def implementation_digest() -> str:
    digest = hashlib.sha256()
    paths = sorted(PACKAGE_ROOT.glob("*.py"))
    for path in paths:
        digest.update(path.name.encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def _distribution_revision(name: str) -> str | None:
    distribution = importlib.metadata.distribution(name)
    try:
        direct_url = json.loads(distribution.read_text("direct_url.json") or "{}")
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{name} has invalid direct_url.json metadata") from error
    revision = direct_url.get("vcs_info", {}).get("commit_id")
    return revision if isinstance(revision, str) else None


def required_env_name(model: str) -> str:
    if model.startswith("openrouter/"):
        return "OPENROUTER_API_KEY"
    if model.startswith("openai/"):
        return "OPENAI_API_KEY"
    if model.startswith("anthropic/"):
        return "ANTHROPIC_API_KEY"
    raise ValueError(f"cannot infer the API-key environment variable for model {model!r}")


def validate_inventory() -> list[str]:
    dataset = complete_dataset()
    ids = [str(sample.id) for sample in dataset]
    if len(ids) != EXPECTED_TOTAL_SAMPLES or len(ids) != len(set(ids)):
        raise RuntimeError(f"the complete harness sample inventory is not {EXPECTED_TOTAL_SAMPLES} unique identities")
    counts = Counter(
        (sample.metadata.get("appa_arm"), sample.metadata.get("task_type"), bool(sample.metadata.get("control")))
        for sample in dataset
    )
    for arm in ARMS:
        for task_type, expected in UPSTREAM_SAMPLE_COUNTS.items():
            if counts[(arm, task_type, False)] != expected:
                raise RuntimeError(f"the pinned {arm}/{task_type} inventory changed")
        if counts[(arm, "data_exfil", True)] != 2:
            raise RuntimeError(f"the paired {arm} Data Exfiltration controls are incomplete")
    return ids


def validate_policies() -> None:
    for security_tool in FIDES_NATIVE_SECURITY_TOOLS:
        ToolDef(security_tool)
    for task_type in TASK_TYPES:
        tools = [*DOMAIN_TOOLS[task_type], RESPONSE_TOOL]
        names = {ToolDef(tool).name for tool in tools}
        fides = fides_policy(task_type, names)
        FidesSession(fides).close()
        FidesSession(fides, native=True).close()
        session = NativeSession(permissive_policy(tools), tools, "preflight")
        session.close()
        if task_type == "data_exfil":
            fixture = AnnotatorFixture(
                [
                    {
                        "name": "Preflight Customer",
                        "id": "CX-PREFLIGHT",
                        "email": "preflight@example.test",
                    }
                ]
            )
            try:
                externals = guarded_externals(fixture.url)
                session = NativeSession(
                    guarded_policy(task_type, acl_readers=fixture.mandate_readers()),
                    tools,
                    "preflight",
                    externals_toml=externals,
                )
                session.close()
            finally:
                fixture.close()
        elif task_type == "memory_poison":
            guarded_tools = [DELEGATE_TOOL, *DOMAIN_TOOLS[task_type], RESPONSE_TOOL]
            session = NativeSession(
                guarded_policy(task_type),
                guarded_tools,
                "preflight",
                spawn_tool="delegate",
            )
            session.close()
        elif task_type == "autonomy_hijack":
            guarded_tools = [DELEGATE_INBOX_TOOL, *DOMAIN_TOOLS[task_type], RESPONSE_TOOL]
            session = NativeSession(
                guarded_policy(task_type),
                guarded_tools,
                "preflight",
                spawn_tool="delegate_inbox",
            )
            session.close()
        else:
            session = NativeSession(guarded_policy(task_type), tools, "preflight")
            session.close()


def preflight(model: str, *, require_credential: bool = True) -> dict[str, object]:
    if importlib.metadata.version("inspect-ai") != INSPECT_AI_VERSION:
        raise RuntimeError(f"inspect-ai must be pinned to {INSPECT_AI_VERSION}")
    if _distribution_revision("inspect-evals") != INSPECT_EVALS_REVISION:
        raise RuntimeError("inspect-evals is not installed from the pinned AgentThreatBench revision")
    if BINDING_IDENTITY != EXPECTED_BINDING_IDENTITY:
        raise RuntimeError(
            f"native binding is {BINDING_IDENTITY!r}, expected {EXPECTED_BINDING_IDENTITY!r}; rebuild the environment"
        )
    if FIDES_VERSION != EXPECTED_FIDES_VERSION:
        raise RuntimeError(
            f"agent-framework-core is {FIDES_VERSION!r}, expected {EXPECTED_FIDES_VERSION!r}; rebuild the environment"
        )
    env_name = required_env_name(model)
    if require_credential and not os.getenv(env_name):
        raise RuntimeError(f"{env_name} is required for {model}")
    sample_ids = validate_inventory()
    validate_policies()
    result = {
        "model": model,
        "required_env": env_name,
        "required_env_set": bool(os.getenv(env_name)),
        "inspect_ai_version": INSPECT_AI_VERSION,
        "inspect_evals_revision": INSPECT_EVALS_REVISION,
        "binding_identity": BINDING_IDENTITY,
        "fides_binding_identity": FIDES_BINDING_IDENTITY,
        "fides_native_binding_identity": FIDES_NATIVE_BINDING_IDENTITY,
        "fides_version": FIDES_VERSION,
        "upstream_samples_per_arm": EXPECTED_UPSTREAM_SAMPLES,
        "custom_controls_per_arm": 2,
        "total_samples": len(sample_ids),
    }
    print(json.dumps(result, indent=2))
    return result


def run_manifest(
    model: str,
    reasoning_effort: str,
    seed: int,
    max_concurrency: int,
    agent_prompt_profile: str = "standard",
    sample_ids: list[str] | None = None,
) -> dict[str, object]:
    if agent_prompt_profile not in AGENT_PROMPT_PROFILES:
        raise ValueError(f"unknown agent prompt profile: {agent_prompt_profile}")
    all_ids = validate_inventory()
    ids = all_ids if sample_ids is None else sample_ids
    if not ids or len(ids) != len(set(ids)) or not set(ids).issubset(all_ids):
        raise ValueError("sample selection must contain unique identities from the complete inventory")
    samples = {str(sample.id): sample for sample in complete_dataset()}
    guarded_exfil_ids = [
        sample_id
        for sample_id in ids
        if samples[sample_id].metadata.get("appa_arm") == "guarded"
        and samples[sample_id].metadata.get("task_type") == "data_exfil"
    ]
    fixture_digests = {
        sample_id: annotator_fixture_digest(
            list(samples[sample_id].metadata.get("customer_db", [])),
        )
        for sample_id in guarded_exfil_ids
    }
    sample_policy_digests = {
        sample_id: hashlib.sha256(
            guarded_policy(
                "data_exfil",
                acl_readers=mandate_readers(list(samples[sample_id].metadata.get("customer_db", []))),
            ).encode()
        ).hexdigest()
        for sample_id in guarded_exfil_ids
    }
    config = {
        "model": model,
        "reasoning_effort": reasoning_effort,
        "seed": seed,
        "agent_prompt_profile": agent_prompt_profile,
        "agent_prompt_sha256": hashlib.sha256(AGENT_PROMPT_PROFILES[agent_prompt_profile].encode()).hexdigest(),
        "inspect_ai_version": INSPECT_AI_VERSION,
        "inspect_evals_revision": INSPECT_EVALS_REVISION,
        "binding_identity": BINDING_IDENTITY,
        "binding_identities": {
            "permissive": BINDING_IDENTITY,
            "guarded": BINDING_IDENTITY,
            "fides": FIDES_BINDING_IDENTITY,
            "fides-native": FIDES_NATIVE_BINDING_IDENTITY,
        },
        "fides_version": FIDES_VERSION,
        "fides_max_concurrent_trajectories": FIDES_MAX_CONCURRENT_TRAJECTORIES,
        "sample_ids": ids,
        "implementation_sha256": implementation_digest(),
        "policy_sha256": {
            arm: {
                task_type: policy_digest(task_type, arm)
                for task_type in TASK_TYPES
                if not (arm == "guarded" and task_type == "data_exfil")
            }
            for arm in ARMS
        },
        "policy_sha256_by_sample": sample_policy_digests,
        "annotator_fixture_sha256": fixture_digests,
    }
    run_digest = hashlib.sha256(json.dumps(config, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    return {
        "format_version": 2,
        "run_digest": run_digest,
        "config": config,
        "execution": {
            "max_samples_values": [max_concurrency],
            "max_connections_values": [max_concurrency],
        },
    }


def ensure_manifest(path: Path, payload: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        existing = json.loads(path.read_text(encoding="utf-8"))
        if (
            existing.get("format_version") != payload["format_version"]
            or existing.get("run_digest") != payload["run_digest"]
            or existing.get("config") != payload["config"]
        ):
            raise ValueError(f"{path.parent} belongs to a different experiment; choose another --run-name")
        old_execution = existing.get("execution", {})
        new_execution = payload["execution"]
        if not isinstance(old_execution, dict) or not isinstance(new_execution, dict):
            raise ValueError(f"{path} has invalid execution metadata")
        merged_execution = {}
        for key in ("max_samples_values", "max_connections_values"):
            old_values = old_execution.get(key, [])
            new_values = new_execution.get(key, [])
            if not isinstance(old_values, list) or not isinstance(new_values, list):
                raise ValueError(f"{path} has invalid {key} metadata")
            if not all(isinstance(value, int) for value in [*old_values, *new_values]):
                raise ValueError(f"{path} has non-integer {key} metadata")
            merged_execution[key] = sorted({*old_values, *new_values})
        payload["execution"] = merged_execution
    temporary = path.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def _score(sample: EvalSample, scorer_suffix: str) -> dict[str, object]:
    matches = [score for name, score in (sample.scores or {}).items() if name.endswith(scorer_suffix)]
    if len(matches) != 1 or not isinstance(matches[0].value, dict):
        raise ValueError(f"sample {sample.id} lacks exactly one {scorer_suffix} score")
    return matches[0].value


def _correct(value: object) -> bool:
    return value == CORRECT


def _sample_cost(sample: EvalSample) -> float | None:
    costs = [usage.total_cost for usage in sample.model_usage.values()]
    if costs and all(cost is not None for cost in costs):
        return sum(costs)

    provider_costs = []
    model_calls = 0
    for event in sample.events:
        if event.event != "model" or event.call is None:
            continue
        model_calls += 1
        call = event.call.model_dump(mode="json")
        response = call.get("response")
        usage = response.get("usage") if isinstance(response, dict) else None
        cost = usage.get("cost") if isinstance(usage, dict) else None
        if isinstance(cost, int | float):
            provider_costs.append(float(cost))
    return sum(provider_costs) if model_calls and len(provider_costs) == model_calls else None


def _transcript_digest(sample: EvalSample) -> str:
    transcript = [message.model_dump(mode="json") for message in sample.messages]
    encoded = json.dumps(transcript, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def _scoreable_limit_termination(sample: EvalSample) -> str | None:
    """Return Inspect's normal scoreable-limit termination, if present."""
    if sample.error is not None:
        return None
    limit = getattr(sample, "limit", None)
    limit_type = getattr(limit, "type", None)
    limits = [event for event in sample.events if event.event == "sample_limit"]
    if isinstance(limit_type, str):
        if limits and (len(limits) != 1 or getattr(limits[0], "type", None) != limit_type):
            return None
    elif len(limits) == 1:
        limit_type = getattr(limits[0], "type", None)
    return f"{limit_type}_limit" if isinstance(limit_type, str) else None


def _audit_diagnostics(
    audit_dir: Path,
    samples: list[EvalSample],
    manifest: dict[str, object],
) -> dict[str, object]:
    paths = sorted(audit_dir.glob("*.json"))
    records = [(path, json.loads(path.read_text(encoding="utf-8"))) for path in paths]
    config = manifest.get("config")
    if not isinstance(config, dict):
        raise ValueError("run manifest has no config object")
    policy_digests = config.get("policy_sha256")
    sample_policy_digests = config.get("policy_sha256_by_sample")
    fixture_digests = config.get("annotator_fixture_sha256")
    if (
        not isinstance(policy_digests, dict)
        or not isinstance(sample_policy_digests, dict)
        or not isinstance(fixture_digests, dict)
    ):
        raise ValueError("run manifest lacks policy or annotator fixture digests")
    agent_prompt_profile = config.get("agent_prompt_profile")
    if not isinstance(agent_prompt_profile, str):
        raise ValueError("run manifest lacks an agent prompt profile")

    selected: list[tuple[Path, dict[str, object]]] = []
    mediated_samples = [sample for sample in samples if sample.metadata.get("appa_arm") != "stock"]
    for sample in mediated_samples:
        if sample.uuid is None:
            raise ValueError(f"sample {sample.id} has no UUID for audit correlation")
        digest = _transcript_digest(sample)
        limit_termination = _scoreable_limit_termination(sample)
        matches = [
            (path, record)
            for path, record in records
            if str(record.get("sample_id")) == str(sample.id)
            and record.get("epoch") == sample.epoch
            and record.get("sample_uuid") == sample.uuid
            and record.get("transcript_sha256") == digest
            and (
                record.get("completed") is True
                or (
                    limit_termination is not None
                    and record.get("completed") is False
                    and record.get("termination") == "exception"
                )
            )
        ]
        if not matches:
            raise ValueError(f"sample {sample.id} has no correlated completed mediation audit")
        path, record = max(matches, key=lambda item: str(item[1].get("written_at", "")))
        if record.get("completed") is False:
            record = {**record, "completed": True, "termination": limit_termination}
        arm = str(sample.metadata.get("appa_arm"))
        task_type = str(sample.metadata.get("task_type"))
        expected_policy = (
            sample_policy_digests.get(str(sample.id))
            if arm == "guarded" and task_type == "data_exfil"
            else policy_digests.get(arm, {}).get(task_type)
        )
        expected_fixture = fixture_digests.get(str(sample.id))
        expected_binding = {
            "fides": FIDES_BINDING_IDENTITY,
            "fides-native": FIDES_NATIVE_BINDING_IDENTITY,
        }.get(arm, BINDING_IDENTITY)
        if record.get("binding_identity") != expected_binding:
            raise ValueError(f"audit {path} has the wrong binding identity for {arm}")
        if record.get("arm") != arm or record.get("task_type") != task_type:
            raise ValueError(f"audit {path} does not match the final sample metadata")
        if record.get("agent_prompt_profile") != agent_prompt_profile:
            raise ValueError(f"audit {path} does not match the manifested agent prompt profile")
        if record.get("policy_sha256") != expected_policy:
            raise ValueError(f"audit {path} does not match the manifested policy")
        if record.get("annotator_fixture_sha256") != expected_fixture:
            raise ValueError(f"audit {path} does not match the manifested annotator fixture")
        selected.append((path, record))

    events = Counter()
    by_arm = defaultdict(Counter)
    terminations = Counter()
    annotator_requests = 0
    fides_audit_entries = 0
    for _, record in selected:
        arm = str(record.get("arm"))
        terminations[str(record.get("termination"))] += 1
        for event in record.get("events", []):
            if isinstance(event, dict) and isinstance(event.get("kind"), str):
                events[event["kind"]] += 1
                by_arm[arm][event["kind"]] += 1
        requests = record.get("annotator_requests", [])
        if isinstance(requests, list):
            annotator_requests += len(requests)
        fides_records = record.get("fides_audit")
        if arm in {"fides", "fides-native"}:
            if not isinstance(fides_records, list):
                raise ValueError("a correlated FIDES audit lacks the official middleware audit log")
            fides_audit_entries += len(fides_records)
        elif fides_records is not None:
            raise ValueError("an OpenAPPA audit unexpectedly contains a FIDES middleware audit log")
    return {
        "attempt_audit_count": len(paths),
        "correlated_final_audit_count": len(selected),
        "discarded_attempt_audit_count": len(paths) - len(selected),
        "events": dict(events),
        "events_by_arm": {arm: dict(counts) for arm, counts in by_arm.items()},
        "terminations": dict(terminations),
        "annotator_request_count": annotator_requests,
        "fides_audit_entry_count": fides_audit_entries,
    }


def build_summary(logs: list[EvalLog], audit_dir: Path, manifest: dict[str, object]) -> dict[str, object]:
    if len(logs) != 1 or logs[0].status != "success" or logs[0].samples is None:
        statuses = [log.status for log in logs]
        raise ValueError(f"the complete evaluation did not produce one successful log: {statuses}")
    samples = logs[0].samples
    config = manifest.get("config")
    selected_ids = config.get("sample_ids") if isinstance(config, dict) else None
    if not isinstance(selected_ids, list) or not all(isinstance(sample_id, str) for sample_id in selected_ids):
        raise ValueError("run manifest has no valid sample selection")
    if len(samples) != len(selected_ids) or {str(sample.id) for sample in samples} != set(selected_ids):
        raise ValueError(f"expected {len(selected_ids)} selected scored samples, found {len(samples)}")

    groups: dict[tuple[str, str, str], list[EvalSample]] = defaultdict(list)
    stock_parity_failures = []
    for sample in samples:
        arm = str(sample.metadata.get("appa_arm"))
        task_type = str(sample.metadata.get("task_type"))
        kind = "controls" if sample.metadata.get("control") else "upstream"
        groups[(arm, task_type, kind)].append(sample)
        if arm == "stock" and kind == "upstream":
            actual = _score(sample, "actual_dispatch_scorer")
            upstream = _score(sample, "agent_threat_bench_scorer")
            if (
                actual["proposal_utility"] != upstream["utility"]
                or actual["proposal_security"] != upstream["security"]
                or not _correct(actual["dispatch_parity"])
            ):
                stock_parity_failures.append(str(sample.metadata.get("original_sample_id")))
    if stock_parity_failures:
        raise ValueError(f"stock proposal/dispatch scoring parity failed: {stock_parity_failures}")

    sample_costs = [_sample_cost(sample) for sample in samples]
    grouped_summary: dict[str, dict[str, dict[str, object]]] = {arm: {} for arm in ARMS}
    for (arm, task_type, kind), grouped in sorted(groups.items()):
        values = [_score(sample, "actual_dispatch_scorer") for sample in grouped]
        key = task_type if kind == "upstream" else f"{task_type}_controls"
        grouped_summary[arm][key] = {
            "samples": len(grouped),
            "actual_utility": sum(_correct(value["actual_utility"]) for value in values) / len(values),
            "actual_security": sum(_correct(value["actual_security"]) for value in values) / len(values),
            "proposal_utility": sum(_correct(value["proposal_utility"]) for value in values) / len(values),
            "proposal_security": sum(_correct(value["proposal_security"]) for value in values) / len(values),
            "dispatch_parity": sum(_correct(value["dispatch_parity"]) for value in values) / len(values),
            "sample_results": {
                str(sample.metadata.get("original_sample_id")): _score(sample, "actual_dispatch_scorer")
                for sample in grouped
            },
        }

    return {
        "format_version": 2,
        "run_digest": manifest["run_digest"],
        "completed_at": datetime.now(UTC).isoformat(),
        "sample_count": len(samples),
        "cost_usd": sum(cost for cost in sample_costs if cost is not None)
        if all(cost is not None for cost in sample_costs)
        else None,
        "stock_actual_dispatch_parity": True,
        "groups": grouped_summary,
        "mediation_audit": _audit_diagnostics(audit_dir, samples, manifest),
        "interpretation": {
            "memory_poison": (
                "The guarded parent fixes a bounded closed schema before retrieval, isolates the memory read in a "
                "separate child model context, and accepts only the child's schema-attested canonical return. Raw "
                "memory never reaches the parent and failures withhold; attestation bounds instruction flow but "
                "does not prove factual truth. The FIDES arm labels the complete memory result untrusted and gates "
                "memory writes and checked delivery; the FIDES-native arm additionally hides untrusted results and "
                "exposes the official quarantine tools."
            ),
            "autonomy_hijack": (
                "The guarded arm labels the complete inbox suspicious, isolates its read in a child, and freezes one "
                "bounded triage record per opaque index before the read. The deployment policy admits only normal and "
                "spam categorizations. Every non-spam record produces a bounded topic/action/urgency draft; an "
                "important candidate is categorized as normal like any other non-spam record. Raw email content never "
                "reaches the parent. Attestation bounds "
                "instruction flow but does not prove semantic truth. The FIDES arm applies the corresponding "
                "untrusted label and gates categorization, summaries, and checked delivery. The FIDES-native arm "
                "additionally uses automatic hiding and quarantined processing."
            ),
            "data_exfil": (
                "The guarded arm annotates source ACLs and exact sink recipients per call without using sensitive "
                "fields or attack markers in policy decisions. FIDES has no equivalent per-recipient audience gate, "
                "so its private-to-public policy conservatively blocks authorized and unauthorized post-read delivery."
            ),
        },
    }


def run_complete(
    model: str,
    reasoning_effort: str,
    max_concurrency: int,
    seed: int,
    logdir: str,
    run_name: str | None,
    *,
    dry_run: bool = False,
    agent_prompt_profile: str = "standard",
    sample_ids: list[str] | None = None,
) -> Path | None:
    preflight(model)
    manifest = run_manifest(model, reasoning_effort, seed, max_concurrency, agent_prompt_profile, sample_ids)
    if dry_run:
        print(json.dumps(manifest, indent=2))
        return None
    if run_name is None:
        stamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
        scope = "smoke" if sample_ids is not None else "full"
        profile = "" if agent_prompt_profile == "standard" else f"-{slug(agent_prompt_profile)}"
        run_name = f"agent-threat-bench-{scope}{profile}-{slug(model)}-{stamp}"
    output_dir = Path(logdir) / slug(run_name)
    ensure_manifest(output_dir / "run-config.json", manifest)
    audit_dir = output_dir / "mediation-audit"
    success, headers = eval_set(
        tasks=[complete_task(audit_dir, agent_prompt_profile)],
        log_dir=str(output_dir / "inspect-logs"),
        model=model,
        reasoning_effort=reasoning_effort,
        seed=seed,
        sample_id=sample_ids,
        max_samples=max_concurrency,
        max_connections=max_concurrency,
        max_tasks=1,
        retry_attempts=3,
        retry_immediate=True,
        retry_on_error=1,
        fail_on_error=True,
        log_samples=True,
        log_model_api=True,
        metadata={"appa_run_digest": manifest["run_digest"]},
    )
    if not success:
        raise RuntimeError("Inspect exhausted task retries before completing AgentThreatBench")
    logs = []
    for header in headers:
        if header.location is None:
            raise RuntimeError("Inspect returned a completed log without a location")
        logs.append(read_eval_log(header.location))
    summary = build_summary(logs, audit_dir, manifest)
    summary_path = output_dir / "summary.json"
    temporary = summary_path.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    temporary.replace(summary_path)
    print(json.dumps({"summary": str(summary_path), "cost_usd": summary["cost_usd"]}, indent=2))
    return output_dir
