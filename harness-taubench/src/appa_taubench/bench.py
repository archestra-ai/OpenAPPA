"""Run a small, reproducible TauBench utility evaluation with and without OpenAPPA."""

import logging
import re
from functools import partial
from pathlib import Path

from tau2.data_model.simulation import TextRunConfig
from tau2.registry import registry
from tau2.runner import build_environment, get_tasks, run_tasks

from appa_taubench.agent import create_appa_agent, drain_stats
from appa_taubench.native import BINDING_IDENTITY
from appa_taubench.policies import load_policy

logger = logging.getLogger(__name__)


def mean(values) -> float | None:
    values = list(values)
    return sum(values) / len(values) if values else None


def slug(value: str) -> str:
    return re.sub(r"[^a-zA-Z0-9_.-]+", "_", value)


def action_coverage(runs) -> tuple[float, int]:
    checks = [check for run in runs if run.reward_info is not None for check in (run.reward_info.action_checks or [])]
    return sum(check.action_reward for check in checks), len(checks)


def run_bench(
    domain: str,
    defenses: list[str],
    task_ids: list[str],
    model: str,
    user_model: str,
    logdir: str,
    run_name: str | None,
    seed: int,
    max_steps: int,
) -> int:
    tasks = get_tasks(domain, task_ids=task_ids)
    tool_names = {tool.name for tool in build_environment(domain).get_tools()}
    policy = load_policy(domain)
    policy.check_covers(tool_names)

    for defense in defenses:
        drain_stats()
        if defense == "appa":
            agent_name = f"appa_agent_{slug(BINDING_IDENTITY)}"
            if agent_name not in registry.get_info().agents:
                factory = partial(create_appa_agent, appa_policy=policy.toml)
                registry.register_agent_factory(factory, agent_name)
        else:
            agent_name = "llm_agent"

        suffix = run_name or f"{domain}-{'-'.join(task_ids)}-{slug(model)}-{defense}"
        if len(defenses) > 1 and run_name:
            suffix = f"{suffix}-{defense}"
        output_dir = Path(logdir) / suffix
        config = TextRunConfig(
            domain=domain,
            agent=agent_name,
            llm_agent=model,
            llm_args_agent={"temperature": 0},
            llm_user=user_model,
            llm_args_user={"temperature": 0},
            task_ids=task_ids,
            num_trials=1,
            max_steps=max_steps,
            max_concurrency=1,
            seed=seed,
            save_to=suffix,
            auto_resume=True,
            auto_review=False,
            hallucination_retries=0,
        )
        config.validate()
        results = run_tasks(
            config,
            tasks,
            save_path=output_dir / "results.json",
            save_dir=output_dir,
        )
        rewards = [run.reward_info.reward for run in results.simulations if run.reward_info is not None]
        costs = [
            (run.agent_cost or 0.0) + (run.user_cost or 0.0)
            for run in results.simulations
            if run.agent_cost is not None or run.user_cost is not None
        ]
        logger.info("%s on %s tasks %s", defense, domain, ", ".join(task_ids))
        logger.info("mean reward: %s", "n/a" if not rewards else f"{mean(rewards):.3f}")
        matched_actions, total_actions = action_coverage(results.simulations)
        logger.info(
            "expected action coverage: %s",
            "n/a" if not total_actions else f"{matched_actions:g}/{total_actions}",
        )
        logger.info("reported model cost: %s", "n/a" if not costs else f"${sum(costs):.4f}")
        for run in results.simulations:
            reward = None if run.reward_info is None else run.reward_info.reward
            matched, total = action_coverage([run])
            logger.info(
                "task %-8s reward=%s actions=%s termination=%s",
                run.task_id,
                "n/a" if reward is None else f"{reward:.3f}",
                "n/a" if not total else f"{matched:g}/{total}",
                run.termination_reason.value,
            )
        if defense == "appa":
            stats = drain_stats()
            if stats.completions:
                logger.info(
                    "OpenAPPA (new episodes): checks=%d allowed=%d policy_blocks=%d "
                    "sequential_blocks=%d admitted_results=%d sealed_results=%d completions=%d",
                    stats.checks,
                    stats.allowed,
                    stats.policy_blocks,
                    stats.sequential_blocks,
                    stats.admitted_results,
                    stats.sealed_results,
                    stats.completions,
                )
            else:
                logger.info("OpenAPPA counters unavailable because every episode was resumed")
        logger.info("results: %s", output_dir / "results.json")
    return 0
