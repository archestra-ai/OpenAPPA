"""Run a reproducible, submission-shaped Tau Knowledge evaluation."""

import hashlib
import importlib.metadata
import importlib.util
import json
import logging
import os
import re
import shutil
import subprocess
import sys
from dataclasses import asdict, dataclass
from functools import partial
from pathlib import Path

from tau2.data_model.simulation import Results, TerminationReason, TextRunConfig
from tau2.registry import registry
from tau2.runner import get_tasks, run_tasks
from tau2.scripts.leaderboard.verify_trajectories_public import check_num_trials, check_tasks

from appa_taubench.agent import create_appa_agent, drain_stats
from appa_taubench.knowledge import DOMAIN, discoverable_tools, model_tools, policy_tool_names
from appa_taubench.native import BINDING_IDENTITY, FrameworkSession
from appa_taubench.policies import Policy, load_policy

logger = logging.getLogger(__name__)

TAU2_REVISION = "93ee97b8303ce0e89e0ad17e6207591a1846f84b"
PACKAGE_ROOT = Path(__file__).resolve().parent
RUN_MANIFEST = "run-config.json"


@dataclass(frozen=True)
class RunSpec:
    retrieval_config: str
    model: str
    user_model: str
    num_trials: int
    max_steps: int
    max_concurrency: int
    seed: int
    policy_sha256: str
    implementation_sha256: str
    binding_identity: str = BINDING_IDENTITY
    tau2_revision: str = TAU2_REVISION
    domain: str = DOMAIN
    task_split_name: str = "base"
    model_args: tuple[tuple[str, int], ...] = (("temperature", 0),)
    user_model_args: tuple[tuple[str, int], ...] = (("temperature", 0),)

    def payload(self) -> dict:
        payload = asdict(self)
        payload["model_args"] = dict(self.model_args)
        payload["user_model_args"] = dict(self.user_model_args)
        return payload

    def digest(self) -> str:
        encoded = json.dumps(self.payload(), sort_keys=True, separators=(",", ":"))
        return hashlib.sha256(encoded.encode()).hexdigest()


def mean(values) -> float | None:
    values = list(values)
    return sum(values) / len(values) if values else None


def slug(value: str) -> str:
    return re.sub(r"[^a-zA-Z0-9_.-]+", "_", value)


def action_coverage(runs) -> tuple[float, int]:
    checks = [check for run in runs if run.reward_info is not None for check in (run.reward_info.action_checks or [])]
    return sum(check.action_reward for check in checks), len(checks)


def implementation_digest() -> str:
    digest = hashlib.sha256()
    paths = sorted(PACKAGE_ROOT.glob("*.py")) + sorted(PACKAGE_ROOT.joinpath("contracts").glob("*.toml"))
    for path in paths:
        digest.update(path.relative_to(PACKAGE_ROOT).as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def make_run_spec(
    retrieval_config: str,
    model: str,
    user_model: str,
    num_trials: int,
    max_steps: int,
    max_concurrency: int,
    seed: int,
    policy: Policy,
) -> RunSpec:
    return RunSpec(
        retrieval_config=retrieval_config,
        model=model,
        user_model=user_model,
        num_trials=num_trials,
        max_steps=max_steps,
        max_concurrency=max_concurrency,
        seed=seed,
        policy_sha256=hashlib.sha256(policy.toml.encode()).hexdigest(),
        implementation_sha256=implementation_digest(),
    )


def ensure_run_manifest(output_dir: Path, spec: RunSpec) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    path = output_dir / RUN_MANIFEST
    payload = {"format_version": 1, "run_digest": spec.digest(), "config": spec.payload()}
    if path.exists():
        existing = json.loads(path.read_text(encoding="utf-8"))
        if existing != payload:
            raise ValueError(f"{path} does not match the requested run; use a different --run-name")
        return
    temporary = path.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def _required_api_keys(retrieval_config: str, models: list[str]) -> set[str]:
    required = {"OPENAI_API_KEY"} if retrieval_config == "alltools" else {"OPENROUTER_API_KEY"}
    for model in models:
        if model.startswith("openrouter/"):
            required.add("OPENROUTER_API_KEY")
        elif model.startswith("openai/") or model.startswith("gpt-"):
            required.add("OPENAI_API_KEY")
        elif model.startswith("anthropic/") or model.startswith("claude-"):
            required.add("ANTHROPIC_API_KEY")
    return required


def _check_checkout_revision() -> None:
    configured_data_dir = os.getenv("TAU2_DATA_DIR")
    if not configured_data_dir:
        raise RuntimeError("TAU2_DATA_DIR is not set; run through appa-taubench or set it explicitly")
    data_dir = Path(configured_data_dir).resolve()
    checkout = data_dir.parent
    try:
        revision = subprocess.run(
            ["git", "-C", str(checkout), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError) as error:
        raise RuntimeError(f"Tau data directory is not inside the pinned Git checkout: {data_dir}") from error
    if revision != TAU2_REVISION:
        raise RuntimeError(f"Tau checkout is {revision}, expected {TAU2_REVISION}; rerun ./setup-taubench.sh")


def _check_tau_distribution_revision() -> None:
    distribution = importlib.metadata.distribution("tau2")
    try:
        direct_url = json.loads(distribution.read_text("direct_url.json") or "{}")
    except json.JSONDecodeError as error:
        raise RuntimeError("the installed Tau package has invalid revision metadata") from error
    installed_revision = direct_url.get("vcs_info", {}).get("commit_id")
    if distribution.version != "1.0.1" or installed_revision != TAU2_REVISION:
        raise RuntimeError("the installed Tau package is not the pinned 1.0.1 revision; rerun ./setup-taubench.sh")


def check_tau_installation() -> None:
    """Require matching pinned Tau code and benchmark data checkouts."""
    _check_checkout_revision()
    _check_tau_distribution_revision()


def preflight(
    retrieval_config: str,
    model: str,
    user_model: str,
    *,
    require_runtime: bool = True,
) -> tuple[list, Policy]:
    """Validate a Knowledge run without constructing retrieval or calling a model."""
    check_tau_installation()
    if importlib.util.find_spec("rank_bm25") is None:
        raise RuntimeError("Tau's knowledge dependencies are missing; rerun ./setup-taubench.sh")

    policy = load_policy()
    policy.check_covers(set(policy_tool_names(retrieval_config)))
    policy_session = FrameworkSession(
        policy.toml,
        model_tools(retrieval_config),
        "Validate the committed Tau Knowledge policy.",
        logical_tools=discoverable_tools(),
    )
    policy_session.close()
    tasks = get_tasks(DOMAIN, task_split_name="base")
    if not tasks or len({task.id for task in tasks}) != len(tasks):
        raise RuntimeError("Tau's banking_knowledge base task set is empty or contains duplicate IDs")

    if require_runtime:
        executables = ["srt", "rg"]
        if sys.platform.startswith("linux"):
            executables.extend(["bwrap", "socat"])
        missing_executables = [name for name in executables if shutil.which(name) is None]
        missing_keys = sorted(
            key for key in _required_api_keys(retrieval_config, [model, user_model]) if not os.getenv(key)
        )
        problems = []
        if missing_executables:
            problems.append(f"missing executables: {', '.join(missing_executables)}")
        if missing_keys:
            problems.append(f"missing environment variables: {', '.join(missing_keys)}")
        if problems:
            raise RuntimeError("Knowledge preflight failed: " + "; ".join(problems))

    logger.info(
        "preflight passed: domain=%s tasks=%d retrieval=%s tau2=%s",
        DOMAIN,
        len(tasks),
        retrieval_config,
        TAU2_REVISION,
    )
    return tasks, policy


def validate_results_for_submission(results: Results) -> None:
    """Apply the public checks plus local publication invariants."""
    tasks_ok, tasks_error = check_tasks(results)
    if not tasks_ok:
        raise ValueError(f"result does not contain the current complete base split: {tasks_error}")
    trials_ok, trials_error = check_num_trials(results)
    if not trials_ok:
        raise ValueError(f"result has incomplete trials: {trials_error}")
    if results.info.num_trials < 4:
        raise ValueError("leaderboard runs require at least four trials")
    infrastructure_errors = [
        run.id for run in results.simulations if run.termination_reason == TerminationReason.INFRASTRUCTURE_ERROR
    ]
    if infrastructure_errors:
        raise ValueError(f"result contains {len(infrastructure_errors)} infrastructure-error simulations")


def run_bench(
    retrieval_config: str,
    model: str,
    user_model: str,
    logdir: str,
    run_name: str | None,
    seed: int,
    max_steps: int,
    max_concurrency: int,
    num_trials: int,
    dry_run: bool = False,
) -> int:
    if num_trials < 4:
        raise ValueError("leaderboard runs require at least four trials")
    tasks, policy = preflight(retrieval_config, model, user_model)
    spec = make_run_spec(
        retrieval_config,
        model,
        user_model,
        num_trials,
        max_steps,
        max_concurrency,
        seed,
        policy,
    )
    base_name = run_name or f"tau-knowledge-{retrieval_config}-{slug(model)}"
    suffix = f"{slug(base_name)}-{spec.digest()[:12]}"
    output_dir = Path(logdir) / suffix
    logger.info(
        "planned run: domain=%s tasks=%d trials=%d simulations=%d retrieval=%s output=%s",
        DOMAIN,
        len(tasks),
        num_trials,
        len(tasks) * num_trials,
        retrieval_config,
        output_dir,
    )
    if dry_run:
        logger.info("dry run complete; Tau was not invoked and no run directory was created")
        return 0
    ensure_run_manifest(output_dir, spec)

    drain_stats()
    output_digest = hashlib.sha256(str(output_dir.resolve()).encode()).hexdigest()[:12]
    agent_name = f"appa_agent_{slug(BINDING_IDENTITY)}_{spec.digest()[:12]}_{output_digest}"
    if agent_name not in registry.get_info().agents:
        factory = partial(
            create_appa_agent,
            appa_policy=policy.toml,
            audit_dir=str(output_dir / "appa-audit"),
        )
        registry.register_agent_factory(factory, agent_name)

    config = TextRunConfig(
        domain=DOMAIN,
        task_split_name="base",
        retrieval_config=retrieval_config,
        agent=agent_name,
        llm_agent=model,
        llm_args_agent=dict(spec.model_args),
        llm_user=user_model,
        llm_args_user=dict(spec.user_model_args),
        task_ids=None,
        num_trials=num_trials,
        max_steps=max_steps,
        max_concurrency=max_concurrency,
        seed=seed,
        save_to=suffix,
        auto_resume=True,
    )
    config.validate()
    results = run_tasks(
        config,
        tasks,
        save_path=output_dir / "results.json",
        save_dir=output_dir,
    )
    validate_results_for_submission(results)

    rewards = [run.reward_info.reward for run in results.simulations if run.reward_info is not None]
    agent_costs = [run.agent_cost for run in results.simulations if run.agent_cost is not None]
    logger.info("OpenAPPA on %d %s tasks with %d trials", len(tasks), DOMAIN, num_trials)
    logger.info("Pass^1 / mean reward: %s", "n/a" if not rewards else f"{mean(rewards):.3f}")
    matched_actions, total_actions = action_coverage(results.simulations)
    logger.info(
        "expected action coverage: %s",
        "n/a" if not total_actions else f"{matched_actions:g}/{total_actions}",
    )
    logger.info(
        "average agent cost per trajectory: %s",
        "n/a" if not agent_costs else f"${mean(agent_costs):.4f}",
    )
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
