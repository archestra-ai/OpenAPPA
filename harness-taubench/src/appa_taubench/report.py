"""Forensic summaries for Tau utility runs and matched controls."""

from __future__ import annotations

import json
from collections import Counter, defaultdict
from pathlib import Path

from tau2.data_model.message import AssistantMessage, ToolMessage, UserMessage
from tau2.data_model.simulation import Results, TerminationReason

RETRIEVAL_TOOLS = {"KB_search_bm25", "KB_search_dense", "shell"}
FAILED_TERMINATIONS = {
    TerminationReason.INFRASTRUCTURE_ERROR,
    TerminationReason.UNEXPECTED_ERROR,
    TerminationReason.AGENT_ERROR,
    TerminationReason.USER_ERROR,
    TerminationReason.CONTEXT_WINDOW_EXCEEDED,
    TerminationReason.TIMEOUT,
    TerminationReason.TOO_MANY_ERRORS,
}


def _atomic_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def finalize_appa_audits(audit_dir: Path, results: Results) -> None:
    """Join each APPA sidecar to Tau's exact scored simulation identity."""
    for simulation in results.simulations:
        path = audit_dir / f"{simulation.id}.json"
        if not path.is_file():
            raise ValueError(f"missing APPA audit for Tau simulation {simulation.id}")
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ValueError(f"invalid APPA audit sidecar: {path}") from error
        if record.get("tau_simulation_id") != simulation.id:
            raise ValueError(f"APPA audit does not identify Tau simulation {simulation.id}")
        expected = {
            "task_id": str(simulation.task_id),
            "trial": simulation.trial,
            "seed": simulation.seed,
        }
        mismatches = [key for key, value in expected.items() if record.get(key) != value]
        if mismatches:
            raise ValueError(f"APPA audit {simulation.id} has mismatched fields: {mismatches}")
        reward = None if simulation.reward_info is None else simulation.reward_info.reward
        record["format_version"] = 2
        record["tau_outcome"] = {
            "simulation_id": simulation.id,
            **expected,
            "termination_reason": simulation.termination_reason.value,
            "reward": reward,
            "successful_tool_results": sum(
                isinstance(message, ToolMessage) and not message.error for message in simulation.get_messages()
            ),
        }
        _atomic_json(path, record)


def _appa_records(audit_dir: Path) -> dict[str, dict]:
    records = {}
    if not audit_dir.is_dir():
        return records
    for path in audit_dir.glob("*.json"):
        record = json.loads(path.read_text(encoding="utf-8"))
        simulation_id = record.get("tau_simulation_id")
        if isinstance(simulation_id, str):
            if simulation_id in records:
                raise ValueError(f"duplicate APPA audit for Tau simulation {simulation_id}")
            records[simulation_id] = record
    return records


def _evaluator_records(audit_dir: Path) -> list[dict]:
    records = []
    if not audit_dir.is_dir():
        return records
    for path in audit_dir.glob("*.json"):
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise ValueError(f"invalid evaluator audit: {path}") from error
        if record.get("format_version") != 1 or record.get("kind") not in {
            "nl_assertion",
            "nl_assertion_preflight",
            "user_review",
        }:
            raise ValueError(f"invalid evaluator audit envelope: {path}")
        request = record.get("request")
        if not isinstance(request, dict) or not isinstance(request.get("model"), str):
            raise ValueError(f"evaluator audit lacks its requested model: {path}")
        if not isinstance(record.get("provider_model"), str):
            raise ValueError(f"evaluator audit lacks its provider-resolved model: {path}")
        records.append(record)
    return records


def validate_evaluator_audits(
    output_dir: Path,
    results: Results,
    judge_model: str,
    review_model: str,
) -> list[dict]:
    """Require exact evaluator evidence for every scored simulation."""
    records = _evaluator_records(output_dir / "evaluator-audit")
    by_call: dict[tuple[str, str], list[dict]] = defaultdict(list)
    preflights = []
    for record in records:
        kind = record["kind"]
        simulation_id = record.get("tau_simulation_id")
        expected_model = review_model if kind == "user_review" else judge_model
        if record["request"]["model"] != expected_model:
            raise ValueError(f"{kind} evaluator used an unexpected model")
        if kind == "nl_assertion_preflight":
            if simulation_id is not None:
                raise ValueError("NL judge preflight was associated with a scored simulation")
            preflights.append(record)
        elif isinstance(simulation_id, str):
            by_call[(simulation_id, kind)].append(record)

    if not preflights:
        raise ValueError("run lacks a successful NL-judge preflight audit")
    for run in results.simulations:
        expected_nl = 1 if str(run.task_id) == "task_102" else 0
        review_records = sorted(by_call[(run.id, "user_review")], key=lambda record: record["timestamp"])
        if not review_records:
            raise ValueError(f"simulation {run.id} has no user_review audit")
        nl_records = sorted(by_call[(run.id, "nl_assertion")], key=lambda record: record["timestamp"])
        if expected_nl and not nl_records:
            raise ValueError(f"simulation {run.id} has no nl_assertion audit")
        if not expected_nl and nl_records:
            raise ValueError(f"simulation {run.id} has unexpected nl_assertion audits")
        review = run.user_only_review
        review_cost = review_records[-1].get("cost")
        if review is not None and review.cost != review_cost:
            raise ValueError(f"simulation {run.id} review cost disagrees with its evaluator audit")

    # Calls whose simulation IDs are absent from Results belong to failed
    # attempts. They remain in the audit and cost totals, but never satisfy a
    # scored simulation's evaluator requirements.
    return records


def _trajectory_diagnostics(simulation) -> dict:
    calls = []
    user_tool_errors = 0
    usage = Counter()
    generation_seconds = 0.0
    agent_calls = 0
    user_calls = 0
    agent_models = set()
    user_models = set()
    for message in simulation.get_messages():
        if isinstance(message, AssistantMessage):
            agent_calls += 1
            if isinstance(message.raw_data, dict) and isinstance(message.raw_data.get("model"), str):
                agent_models.add(message.raw_data["model"])
            for call in message.tool_calls or []:
                if call.name in RETRIEVAL_TOOLS:
                    calls.append((call.name, json.dumps(call.arguments, sort_keys=True)))
        elif isinstance(message, UserMessage):
            user_calls += 1
            if isinstance(message.raw_data, dict) and isinstance(message.raw_data.get("model"), str):
                user_models.add(message.raw_data["model"])
        elif isinstance(message, ToolMessage) and message.requestor == "user" and message.error:
            user_tool_errors += 1
        if isinstance(getattr(message, "usage", None), dict):
            for key, value in message.usage.items():
                if isinstance(value, int | float):
                    usage[key] += value
        generation_seconds += getattr(message, "generation_time_seconds", None) or 0.0
    counts = Counter(calls)
    return {
        "retrieval_calls": len(calls),
        "dense_retrieval_calls": sum(name == "KB_search_dense" for name, _ in calls),
        "repeated_retrieval_calls": sum(count - 1 for count in counts.values()),
        "user_tool_errors": user_tool_errors,
        "visible_agent_model_calls": agent_calls,
        "user_model_calls": user_calls,
        "agent_provider_models": sorted(agent_models),
        "user_provider_models": sorted(user_models),
        "token_usage": dict(usage),
        "generation_seconds": generation_seconds,
    }


def _cost(record: dict) -> float:
    value = record.get("cost")
    return value if isinstance(value, int | float) else 0.0


def _appa_completion_cost(record: dict) -> float:
    return sum(
        event.get("response", {}).get("cost") or 0
        for event in record.get("events", [])
        if event.get("kind") == "model_completion" and isinstance(event.get("response"), dict)
    )


def build_run_summary(
    output_dir: Path,
    results: Results,
    policy_mode: str,
    run_config: dict,
    evaluator_records: list[dict],
) -> dict:
    """Separate utility, participant, evaluator, policy, retrieval, and cost evidence."""
    appa = _appa_records(output_dir / "appa-audit")
    terminations = Counter(run.termination_reason.value for run in results.simulations)
    scored_ids = {run.id for run in results.simulations}
    outcomes = []
    for run in results.simulations:
        audit = appa.get(run.id, {})
        review = run.user_only_review
        reward = None if run.reward_info is None else run.reward_info.reward
        diagnostics = _trajectory_diagnostics(run)
        stats = audit.get("stats", {})
        hidden_cost = stats.get("hidden_cost", 0)
        if audit:
            diagnostics["visible_agent_model_calls"] = stats.get("completions", 0) - stats.get("hidden_completions", 0)
        outcomes.append(
            {
                "simulation_id": run.id,
                "task_id": str(run.task_id),
                "trial": run.trial,
                "seed": run.seed,
                "duration_seconds": run.duration,
                "termination_reason": run.termination_reason.value,
                "execution_failure": run.termination_reason in FAILED_TERMINATIONS,
                "reward": reward,
                "agent_cost": run.agent_cost,
                "visible_agent_cost": None if run.agent_cost is None else run.agent_cost - hidden_cost,
                "hidden_agent_cost": hidden_cost,
                "user_cost": run.user_cost,
                "review_cost": None if review is None else review.cost,
                "user_review_errors": None if review is None else len(review.errors),
                "critical_user_error": None if review is None else review.critical_user_error,
                "policy_checks": stats.get("checks", 0),
                "policy_blocks": stats.get("policy_blocks", 0),
                "hidden_agent_completions": stats.get("hidden_completions", 0),
                "terminal_policy_refusals": sum(
                    event.get("kind") == "terminal_refusal" for event in audit.get("events", [])
                ),
                **diagnostics,
            }
        )

    rewards = [outcome["reward"] for outcome in outcomes if outcome["reward"] is not None]
    evaluator_costs = Counter()
    provider_models: dict[str, set[str]] = defaultdict(set)
    for outcome in outcomes:
        provider_models["agent"].update(outcome["agent_provider_models"])
        provider_models["user"].update(outcome["user_provider_models"])
    for record in appa.values():
        for event in record.get("events", []):
            response = event.get("response")
            raw_data = response.get("raw_data") if isinstance(response, dict) else None
            if event.get("kind") == "model_completion" and isinstance(raw_data, dict):
                model = raw_data.get("model")
                if isinstance(model, str):
                    provider_models["agent"].add(model)
    for record in evaluator_records:
        kind = record["kind"]
        simulation_id = record.get("tau_simulation_id")
        bucket = kind if simulation_id is None or simulation_id in scored_ids else f"discarded_{kind}"
        evaluator_costs[bucket] += _cost(record)
        provider_models[kind].add(record["provider_model"])
    payload = {
        "format_version": 1,
        "status": "validated",
        "policy_mode": policy_mode,
        "run_config": run_config,
        "provider_models": {key: sorted(values) for key, values in sorted(provider_models.items())},
        "simulation_count": len(outcomes),
        "utility": {
            "mean_reward": None if not rewards else sum(rewards) / len(rewards),
            "passes": sum(reward == 1 for reward in rewards),
            "evaluated": len(rewards),
        },
        "terminations": dict(sorted(terminations.items())),
        "execution_failures": sum(outcome["execution_failure"] for outcome in outcomes),
        "policy": {
            "checks": sum(outcome["policy_checks"] for outcome in outcomes),
            "blocks": sum(outcome["policy_blocks"] for outcome in outcomes),
            "hidden_completions": sum(outcome["hidden_agent_completions"] for outcome in outcomes),
            "terminal_refusals": sum(outcome["terminal_policy_refusals"] for outcome in outcomes),
        },
        "retrieval": {
            "calls": sum(outcome["retrieval_calls"] for outcome in outcomes),
            "dense_calls": sum(outcome["dense_retrieval_calls"] for outcome in outcomes),
            "repeated_calls": sum(outcome["repeated_retrieval_calls"] for outcome in outcomes),
            "embedding_cost_usd": None,
            "embedding_cost_note": "Tau 1.0.1 does not expose embedding response usage or cost",
        },
        "user_simulator": {
            "tool_errors": sum(outcome["user_tool_errors"] for outcome in outcomes),
            "reviewed": sum(outcome["user_review_errors"] is not None for outcome in outcomes),
            "episodes_with_errors": sum(bool(outcome["user_review_errors"]) for outcome in outcomes),
            "critical_errors": sum(outcome["critical_user_error"] is True for outcome in outcomes),
        },
        "model_calls": {
            "visible_agent": sum(outcome["visible_agent_model_calls"] for outcome in outcomes),
            "hidden_agent": sum(outcome["hidden_agent_completions"] for outcome in outcomes),
            "user": sum(outcome["user_model_calls"] for outcome in outcomes),
            "user_review": sum(record["kind"] == "user_review" for record in evaluator_records),
            "nl_assertion_judge": sum(record["kind"] == "nl_assertion" for record in evaluator_records),
            "nl_assertion_preflight": sum(record["kind"] == "nl_assertion_preflight" for record in evaluator_records),
            "discarded_agent_attempts": sum(
                record.get("stats", {}).get("completions", 0)
                for simulation_id, record in appa.items()
                if simulation_id not in scored_ids
            ),
        },
        "cost_usd": {
            "agent": sum(outcome["agent_cost"] or 0 for outcome in outcomes),
            "visible_agent": sum(outcome["visible_agent_cost"] or 0 for outcome in outcomes),
            "hidden_agent": sum(outcome["hidden_agent_cost"] or 0 for outcome in outcomes),
            "user": sum(outcome["user_cost"] or 0 for outcome in outcomes),
            "user_review": evaluator_costs["user_review"],
            "nl_assertion_judge": evaluator_costs["nl_assertion"],
            "nl_assertion_preflight": evaluator_costs["nl_assertion_preflight"],
            "discarded_attempt_evaluators": sum(
                value for key, value in evaluator_costs.items() if key.startswith("discarded_")
            ),
            "discarded_agent_attempts": sum(
                _appa_completion_cost(record)
                for simulation_id, record in appa.items()
                if simulation_id not in scored_ids
            ),
        },
        "duration_seconds": sum(outcome["duration_seconds"] for outcome in outcomes),
        "outcomes": outcomes,
    }
    _atomic_json(output_dir / "run-summary.json", payload)
    return payload


def validate_run_integrity(results: Results, task_count: int, num_trials: int) -> None:
    """Reject incomplete, unevaluated, malformed, or infrastructure-failed runs."""
    expected = task_count * num_trials
    if len(results.simulations) != expected:
        raise ValueError(f"run contains {len(results.simulations)} simulations, expected {expected}")
    identities = {(run.task_id, run.trial, run.seed) for run in results.simulations}
    if len(identities) != expected:
        raise ValueError("run contains duplicate task/trial/seed identities")
    infrastructure = [
        run.id
        for run in results.simulations
        if run.termination_reason in {TerminationReason.INFRASTRUCTURE_ERROR, TerminationReason.UNEXPECTED_ERROR}
    ]
    if infrastructure:
        raise ValueError(f"run contains {len(infrastructure)} infrastructure-error simulations")
    for run in results.simulations:
        if run.reward_info is None:
            raise ValueError(f"simulation {run.id} has no evaluator result")
        review = run.user_only_review
        if review is None:
            raise ValueError(f"simulation {run.id} has no user-simulator review")
        if any(error.turn_idx is None or error.turn_idx < 0 for error in review.errors):
            raise ValueError(f"simulation {run.id} has an invalid user-simulator review")


def build_matched_summary(arm_dirs: dict[str, Path], output: Path) -> dict:
    """Compare arms while keeping policy association distinct from causation."""
    summaries = {
        mode: json.loads((directory / "run-summary.json").read_text(encoding="utf-8"))
        for mode, directory in arm_dirs.items()
    }
    identities = {
        mode: {(item["task_id"], item["trial"], item["seed"]): item for item in summary["outcomes"]}
        for mode, summary in summaries.items()
    }
    expected = set(identities["guarded"])
    for mode, by_identity in identities.items():
        if set(by_identity) != expected:
            raise ValueError(f"{mode} arm does not have the same task/trial/seed identities")
    comparisons = []
    for identity, guarded in identities["guarded"].items():
        rewards = {mode: values[identity]["reward"] for mode, values in identities.items()}
        if any(not isinstance(reward, int | float) for reward in rewards.values()):
            raise ValueError(f"matched simulation {identity} lacks a numeric reward")
        comparisons.append(
            {
                "task_id": identity[0],
                "trial": identity[1],
                "seed": identity[2],
                "rewards": rewards,
                "guarded_policy_blocks": guarded["policy_blocks"],
                "guarded_minus_permissive": rewards["guarded"] - rewards["permissive"],
                "policy_associated_difference": (
                    rewards["guarded"] != rewards["permissive"] and guarded["policy_blocks"] > 0
                ),
            }
        )
    payload = {
        "format_version": 1,
        "arms": {mode: str(directory) for mode, directory in arm_dirs.items()},
        "method": "matched task/trial/seed; association is not a causal claim across stochastic trajectories",
        "mean_rewards": {mode: summary["utility"]["mean_reward"] for mode, summary in summaries.items()},
        "cost_usd": {mode: summary["cost_usd"] for mode, summary in summaries.items()},
        "comparisons": comparisons,
    }
    _atomic_json(output, payload)
    return payload
