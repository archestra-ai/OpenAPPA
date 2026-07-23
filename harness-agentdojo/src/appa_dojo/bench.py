"""Run AgentDojo's benchmark with APPA in the tool execution loop."""

import json
import logging
from pathlib import Path

from agentdojo.task_suite.load_suites import get_suite

from appa_dojo.defense import EXECUTE_REMEDY_PLAN, POLICY_BLOCK_SENTINEL
from appa_dojo.pipeline import build_pipeline
from appa_dojo.policies import load_policy

logger = logging.getLogger(__name__)


def policy_name_for(suite_name: str, defense: str) -> str:
    match defense:
        case "none" | "appa-open":
            return f"{suite_name}-open"
        case "appa-practical":
            return f"{suite_name}-practical"
        case "appa-complete":
            return f"{suite_name}-complete"
        case "appa":
            return suite_name
        case _:
            raise ValueError(f"unsupported defense {defense!r}")


def mean(values) -> float | None:
    values = list(values)
    return sum(values) / len(values) if values else None


def episode_files(
    logdir: Path,
    pipeline_name: str,
    suite_name: str,
    results,
    attack_name: str,
) -> list[Path]:
    base = logdir / pipeline_name / suite_name
    files = []
    for user_task_id, injection_task_id in results["utility_results"]:
        injection = injection_task_id or "none"
        path = base / user_task_id / attack_name / f"{injection}.json"
        if path.exists():
            files.append(path)
    return files


def policy_counts(result_files: list[Path]) -> tuple[int, int]:
    blocked = 0
    remedies = 0
    for result_file in result_files:
        result = json.loads(result_file.read_text())
        for message in result.get("messages", []):
            error = message.get("error")
            if isinstance(error, str) and error.startswith(POLICY_BLOCK_SENTINEL):
                blocked += 1
            call = message.get("tool_call")
            if isinstance(call, dict) and call.get("function") == EXECUTE_REMEDY_PLAN:
                remedies += 1
    return blocked, remedies


def percent(value: float | None) -> str:
    return "n/a" if value is None else f"{100 * value:.1f}%"


def grouped_means(results: dict[tuple[str, str], bool], key_index: int) -> dict[str, float]:
    groups: dict[str, list[bool]] = {}
    for key, value in results.items():
        groups.setdefault(key[key_index], []).append(value)
    return {key: mean(values) or 0.0 for key, values in sorted(groups.items())}


def run_bench(
    suite_name: str,
    benchmark_version: str,
    model: str,
    attack_name: str,
    defense: str,
    user_tasks: list[str] | None,
    injection_tasks: list[str] | None,
    logdir: str,
    skip_clean_utility: bool,
) -> int:
    import agentdojo.attacks  # noqa: F401
    from agentdojo.attacks.attack_registry import load_attack
    from agentdojo.benchmark import (
        benchmark_suite_with_injections,
        benchmark_suite_without_injections,
    )
    from agentdojo.logging import OutputLogger

    from appa_dojo import _agentdojo_compat

    _agentdojo_compat.apply()
    suite = get_suite(benchmark_version, suite_name)
    policy_name = policy_name_for(suite_name, defense)
    policy = load_policy(policy_name)
    policy.check_covers({tool.name for tool in suite.tools})
    built = build_pipeline(model, defense, policy)
    pipeline = built.pipeline
    attack = load_attack(attack_name, suite, pipeline)
    logdir_path = Path(logdir)
    clean_utility = None
    run_files: list[Path] = []

    try:
        with OutputLogger(str(logdir_path)):
            if not skip_clean_utility:
                clean = benchmark_suite_without_injections(
                    pipeline,
                    suite,
                    logdir=logdir_path,
                    force_rerun=False,
                    user_tasks=user_tasks,
                    benchmark_version=benchmark_version,
                )
                clean_utility = mean(clean["utility_results"].values())
                run_files.extend(episode_files(logdir_path, pipeline.name, suite_name, clean, "none"))

            attacked = benchmark_suite_with_injections(
                pipeline,
                suite,
                attack,
                logdir=logdir_path,
                force_rerun=False,
                user_tasks=user_tasks,
                injection_tasks=injection_tasks,
                benchmark_version=benchmark_version,
            )
            run_files.extend(
                episode_files(
                    logdir_path,
                    pipeline.name,
                    suite_name,
                    attacked,
                    attack_name,
                )
            )
    finally:
        built.close()

    blocked, remedies = policy_counts(run_files)
    logger.info(
        "%s vs %s on %s (%s)",
        pipeline.name,
        attack_name,
        suite_name,
        benchmark_version,
    )
    logger.info("clean utility: %s", percent(clean_utility))
    logger.info(
        "utility under attack: %s",
        percent(mean(attacked["utility_results"].values())),
    )
    logger.info(
        "attack success rate: %s",
        percent(mean(attacked["security_results"].values())),
    )
    logger.info(
        "injection calibration: %s",
        percent(mean(attacked["injection_tasks_utility_results"].values())),
    )
    for injection_task, successful in sorted(attacked["injection_tasks_utility_results"].items()):
        logger.info("calibration %-12s %s", injection_task, successful)
    for injection_task, asr in grouped_means(attacked["security_results"], 1).items():
        logger.info("ASR %-20s %s", injection_task, percent(asr))
    for injection_task, utility in grouped_means(attacked["utility_results"], 1).items():
        logger.info("attacked utility %-12s %s", injection_task, percent(utility))
    for user_task, utility in grouped_means(attacked["utility_results"], 0).items():
        logger.info("attacked utility %-12s %s", user_task, percent(utility))
    logger.info("policy-blocked calls: %d", blocked)
    logger.info("remedy attempts: %d", remedies)
    logger.info("logs: %s", logdir_path / pipeline.name / suite_name)
    return 0
