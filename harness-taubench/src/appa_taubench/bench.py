"""Run a reproducible, submission-shaped Tau Knowledge evaluation."""

import hashlib
import importlib.metadata
import importlib.util
import json
import logging
import os
import random
import re
import shutil
import subprocess
import sys
from dataclasses import asdict, dataclass
from functools import partial
from pathlib import Path

from tau2.data_model.simulation import Results, TerminationReason, TextRunConfig
from tau2.data_model.tasks import RewardType
from tau2.registry import registry
from tau2.runner import get_tasks, run_tasks
from tau2.scripts.leaderboard.verify_trajectories_public import check_num_trials, check_tasks
from tau2.utils import llm_utils as tau_llm_utils

from appa_taubench.agent import create_appa_agent, drain_stats
from appa_taubench.evaluation import (
    TASK_102_ASSERTION,
    audit_task_102_atomic,
    evaluator_session,
    preflight_nl_judge,
)
from appa_taubench.knowledge import DOMAIN, discoverable_tools, model_tools, policy_tool_names
from appa_taubench.native import BINDING_IDENTITY, Allowed, Blocked, FrameworkSession
from appa_taubench.policies import Policy, load_policy
from appa_taubench.report import (
    build_run_summary,
    finalize_appa_audits,
    validate_evaluator_audits,
    validate_run_integrity,
)

logger = logging.getLogger(__name__)

_tau_to_litellm_messages = tau_llm_utils.to_litellm_messages


def _standard_litellm_messages(messages):
    """Remove Tau's nonstandard duplicate tool name from provider history."""
    provider_messages = _tau_to_litellm_messages(messages)
    for message in provider_messages:
        for tool_call in message.get("tool_calls") or []:
            tool_call.pop("name", None)
    return provider_messages


# The pinned Tau serializer emits both ``tool_call.name`` and the standard
# ``tool_call.function.name``. OpenAI ignores the duplicate, but Mistral
# rejects it. Install the standards-shaped serializer for every participant
# so guarded, permissive, and stock arms retain the same provider boundary.
tau_llm_utils.to_litellm_messages = _standard_litellm_messages

TAU2_REVISION = "93ee97b8303ce0e89e0ad17e6207591a1846f84b"
PACKAGE_ROOT = Path(__file__).resolve().parent
RUN_MANIFEST = "run-config.json"
PILOT_TASK_IDS = (
    "task_001",
    "task_004",
    "task_010",
    "task_026",
    "task_032",
    "task_046",
    "task_050",
    "task_055",
    "task_072",
    "task_102",
)
CHAOS_SCREEN_TASK_IDS = (
    "task_005",
    "task_036",
    "task_075",
)


@dataclass(frozen=True)
class RunSpec:
    retrieval_config: str
    policy_mode: str
    model: str
    user_model: str
    judge_model: str
    review_model: str
    num_trials: int
    max_steps: int
    max_concurrency: int
    seed: int
    trial_seeds: tuple[int, ...]
    task_ids: tuple[str, ...]
    publication_run: bool
    policy_sha256: str
    implementation_sha256: str
    retrieval_index_sha256: str
    binding_identity: str = BINDING_IDENTITY
    tau2_revision: str = TAU2_REVISION
    domain: str = DOMAIN
    task_split_name: str = "base"
    model_args: tuple[tuple[str, object], ...] = (("reasoning_effort", "high"),)
    user_model_args: tuple[tuple[str, object], ...] = (("reasoning_effort", "low"),)
    judge_model_args: tuple[tuple[str, object], ...] = (("temperature", 0),)
    review_model_args: tuple[tuple[str, object], ...] = (("temperature", 0),)
    max_retries: int = 0
    auto_review: bool = True
    review_mode: str = "user"
    verbose_logs: bool = True
    hallucination_retries: int = 0

    def payload(self) -> dict:
        payload = asdict(self)
        payload["task_ids"] = list(self.task_ids)
        payload["trial_seeds"] = list(self.trial_seeds)
        payload["model_args"] = dict(self.model_args)
        payload["user_model_args"] = dict(self.user_model_args)
        payload["judge_model_args"] = dict(self.judge_model_args)
        payload["review_model_args"] = dict(self.review_model_args)
        return payload

    def digest(self) -> str:
        encoded = json.dumps(self.payload(), sort_keys=True, separators=(",", ":"))
        return hashlib.sha256(encoded.encode()).hexdigest()


def mean(values) -> float | None:
    values = list(values)
    return sum(values) / len(values) if values else None


def slug(value: str) -> str:
    return re.sub(r"[^a-zA-Z0-9_.-]+", "_", value)


def trial_seeds(seed: int, num_trials: int) -> tuple[int, ...]:
    generator = random.Random(seed)
    seeds = tuple(generator.randint(0, 1_000_000) for _ in range(num_trials))
    if len(set(seeds)) != num_trials:
        raise ValueError("Tau generated duplicate trial seeds; select a different run seed")
    return seeds


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


def retrieval_index_digest(retrieval_config: str) -> str:
    """Identify the pinned retrieval corpus and index recipe without hashing derived vectors."""
    data_dir = Path(os.environ["TAU2_DATA_DIR"])
    domain_dir = data_dir / "tau2" / "domains" / DOMAIN
    digest = hashlib.sha256()
    digest.update(retrieval_config.encode())
    digest.update(b"\0")
    for path in sorted(domain_dir.rglob("*")):
        if not path.is_file():
            continue
        digest.update(path.relative_to(domain_dir).as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def make_run_spec(
    retrieval_config: str,
    policy_mode: str,
    model: str,
    reasoning_effort: str,
    user_model: str,
    judge_model: str,
    review_model: str,
    num_trials: int,
    max_steps: int,
    max_concurrency: int,
    seed: int,
    task_ids: tuple[str, ...],
    publication_run: bool,
    policy: Policy,
) -> RunSpec:
    return RunSpec(
        retrieval_config=retrieval_config,
        policy_mode=policy_mode,
        model=model,
        model_args=(("reasoning_effort", reasoning_effort),),
        user_model=user_model,
        judge_model=judge_model,
        review_model=review_model,
        num_trials=num_trials,
        max_steps=max_steps,
        max_concurrency=max_concurrency,
        seed=seed,
        trial_seeds=trial_seeds(seed, num_trials),
        task_ids=task_ids,
        publication_run=publication_run,
        policy_sha256=hashlib.sha256(policy.toml.encode()).hexdigest(),
        implementation_sha256=implementation_digest(),
        retrieval_index_sha256=retrieval_index_digest(retrieval_config),
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


def validate_task_inventory(tasks: list) -> None:
    """Pin the complete task/evaluator surface the contract was reviewed against."""
    actions = [
        action
        for task in tasks
        if task.evaluation_criteria is not None
        for action in (task.evaluation_criteria.actions or [])
    ]
    assistant_actions = [action for action in actions if action.requestor == "assistant"]
    user_actions = [action for action in actions if action.requestor == "user"]
    nl_tasks = [
        task
        for task in tasks
        if task.evaluation_criteria is not None and RewardType.NL_ASSERTION in task.evaluation_criteria.reward_basis
    ]
    if (len(tasks), len(actions), len(assistant_actions), len(user_actions)) != (97, 955, 853, 102):
        raise RuntimeError("the pinned Tau task/action inventory changed")
    if [str(task.id) for task in nl_tasks] != ["task_102"]:
        raise RuntimeError("the pinned Tau NL-assertion inventory changed")
    if (nl_tasks[0].evaluation_criteria.nl_assertions or []) != [TASK_102_ASSERTION]:
        raise RuntimeError("the pinned task-102 NL assertion changed")
    if any(task.initial_state is not None and task.initial_state.message_history for task in tasks):
        raise RuntimeError("APPA's text agent requires the pinned tasks to begin without message history")


def validate_golden_actions(tasks: list) -> None:
    """Fail preflight when Tau cannot replay any task's evaluator actions."""
    environment_constructor = registry.get_env_constructor(DOMAIN)
    failures = []
    for task in tasks:
        environment = environment_constructor(retrieval_variant="no_knowledge", task=task)
        initial = task.initial_state
        environment.set_state(
            initialization_data=None if initial is None else initial.initialization_data,
            initialization_actions=None if initial is None else initial.initialization_actions,
            message_history=[] if initial is None else initial.message_history or [],
            strict=True,
        )
        criteria = task.evaluation_criteria
        for action in [] if criteria is None else criteria.actions or []:
            try:
                environment.make_tool_call(
                    tool_name=action.name,
                    requestor=action.requestor,
                    **action.arguments,
                )
            except Exception as error:  # Tau's evaluator otherwise logs and continues.
                failures.append(f"{task.id}:{action.name}: {error}")
                break
    if failures:
        raise RuntimeError("golden-action replay failed: " + "; ".join(failures))


def validate_golden_policy_actions(tasks: list, policy: Policy, retrieval_config: str) -> None:
    """Replay all expected assistant calls through the committed APPA contract."""
    environment_constructor = registry.get_env_constructor(DOMAIN)
    failures = []
    for task in tasks:
        environment = environment_constructor(retrieval_variant="no_knowledge", task=task)
        initial = task.initial_state
        environment.set_state(
            initialization_data=None if initial is None else initial.initialization_data,
            initialization_actions=None if initial is None else initial.initialization_actions,
            message_history=[] if initial is None else initial.message_history or [],
            strict=True,
        )
        session = FrameworkSession(
            policy.toml,
            model_tools(retrieval_config),
            str(task.user_scenario),
            logical_tools=discoverable_tools(),
        )
        first_completion = True
        action_name = "<initialization>"
        try:
            criteria = task.evaluation_criteria
            for action in [] if criteria is None else criteria.actions or []:
                action_name = action.name
                if action.requestor != "assistant":
                    environment.make_tool_call(
                        tool_name=action.name,
                        requestor=action.requestor,
                        **action.arguments,
                    )
                    continue
                if not first_completion:
                    session.new_round()
                first_completion = False
                decision = session.check(action.name, action.arguments)
                if isinstance(decision, Blocked):
                    plan = re.search(r"remedy-\d+", decision.feedback)
                    if not decision.recoverable or plan is None:
                        raise RuntimeError("contract offered no executable remedy")
                    session.new_round()
                    decision = session.check("execute_remedy_plan", {"plan_id": plan.group()})
                if not isinstance(decision, Allowed):
                    raise RuntimeError("contract did not authorize expected action")
                result = environment.make_tool_call(
                    tool_name=decision.dispatched_tool,
                    requestor="assistant",
                    **decision.dispatched_arguments,
                )
                session.report(str(result), error=False)
        except Exception as error:
            failures.append(f"{task.id}:{action_name}: {error}")
        finally:
            session.close()
    if failures:
        raise RuntimeError("golden APPA-flow replay failed: " + "; ".join(failures))


def preflight(
    retrieval_config: str,
    model: str,
    user_model: str,
    judge_model: str | None = None,
    review_model: str | None = None,
    policy_mode: str = "guarded",
    *,
    require_runtime: bool = True,
) -> tuple[list, Policy]:
    """Validate a Knowledge run without constructing retrieval or calling a model."""
    check_tau_installation()
    if importlib.util.find_spec("rank_bm25") is None:
        raise RuntimeError("Tau's knowledge dependencies are missing; rerun ./setup-taubench.sh")

    tool_names = policy_tool_names(retrieval_config)
    policy = load_policy(policy_mode, tool_names)
    policy.check_covers(set(tool_names))
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
    validate_task_inventory(tasks)
    validate_golden_actions(tasks)
    if policy_mode == "guarded":
        validate_golden_policy_actions(tasks, policy, retrieval_config)

    if require_runtime:
        executables = ["srt", "rg"]
        if sys.platform.startswith("linux"):
            executables.extend(["bwrap", "socat"])
        missing_executables = [name for name in executables if shutil.which(name) is None]
        missing_keys = sorted(
            key
            for key in _required_api_keys(
                retrieval_config,
                [model, user_model, judge_model or model, review_model or judge_model or model],
            )
            if not os.getenv(key)
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


def _execute_bench(
    retrieval_config: str,
    policy_mode: str,
    model: str,
    reasoning_effort: str,
    user_model: str,
    judge_model: str,
    review_model: str,
    logdir: str,
    run_name: str | None,
    seed: int,
    max_steps: int,
    max_concurrency: int,
    num_trials: int,
    task_ids: tuple[str, ...] | None,
    publication_run: bool,
    dry_run: bool = False,
) -> Path | None:
    if publication_run and num_trials < 4:
        raise ValueError("leaderboard runs require at least four trials")
    all_tasks, policy = preflight(
        retrieval_config,
        model,
        user_model,
        judge_model,
        review_model,
        policy_mode,
    )
    available = {str(task.id): task for task in all_tasks}
    selected_ids = tuple(available) if task_ids is None else task_ids
    missing = sorted(set(selected_ids) - set(available))
    if missing:
        raise ValueError(f"unknown Tau task IDs: {missing}")
    tasks = [available[task_id] for task_id in selected_ids]
    if publication_run and len(tasks) != len(all_tasks):
        raise ValueError("leaderboard runs require the complete base split")
    spec = make_run_spec(
        retrieval_config,
        policy_mode,
        model,
        reasoning_effort,
        user_model,
        judge_model,
        review_model,
        num_trials,
        max_steps,
        max_concurrency,
        seed,
        selected_ids,
        publication_run,
        policy,
    )
    base_name = run_name or f"tau-knowledge-{policy_mode}-{retrieval_config}-{slug(model)}"
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
        return None
    ensure_run_manifest(output_dir, spec)
    drain_stats()
    if policy_mode == "stock":
        agent_name = "llm_agent"
    else:
        output_digest = hashlib.sha256(str(output_dir.resolve()).encode()).hexdigest()[:12]
        agent_name = f"appa_agent_{slug(BINDING_IDENTITY)}_{spec.digest()[:12]}_{output_digest}"
        if agent_name not in registry.get_info().agents:
            factory = partial(
                create_appa_agent,
                appa_policy=policy.toml,
                audit_dir=str(output_dir / "appa-audit"),
                trial_seeds=spec.trial_seeds,
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
        task_ids=None if publication_run else list(selected_ids),
        num_trials=num_trials,
        max_steps=max_steps,
        max_concurrency=max_concurrency,
        seed=seed,
        save_to=suffix,
        auto_resume=True,
        auto_review=spec.auto_review,
        review_mode=spec.review_mode,
        review_model=review_model,
        max_retries=spec.max_retries,
        verbose_logs=spec.verbose_logs,
        hallucination_retries=spec.hallucination_retries,
    )
    if not config.verbose_logs:
        raise RuntimeError("verbose Tau artifacts are required for direct simulation audit correlation")
    config.validate()
    with evaluator_session(
        judge_model,
        dict(spec.judge_model_args),
        dict(spec.review_model_args),
        output_dir / "evaluator-audit",
    ):
        preflight_nl_judge(available["task_102"])
        results = run_tasks(
            config,
            tasks,
            save_path=output_dir / "results.json",
            save_dir=output_dir,
        )
        audit_task_102_atomic(results, judge_model, dict(spec.judge_model_args))

    validate_run_integrity(results, len(tasks), num_trials)
    evaluator_records = validate_evaluator_audits(output_dir, results, judge_model, review_model)
    if publication_run:
        validate_results_for_submission(results)
    if policy_mode != "stock":
        finalize_appa_audits(output_dir / "appa-audit", results)
    summary = build_run_summary(output_dir, results, policy_mode, spec.payload(), evaluator_records)

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
            "sequential_blocks=%d hidden_completions=%d hidden_cost=$%.4f "
            "admitted_results=%d sealed_results=%d completions=%d",
            stats.checks,
            stats.allowed,
            stats.policy_blocks,
            stats.sequential_blocks,
            stats.hidden_completions,
            stats.hidden_cost,
            stats.admitted_results,
            stats.sealed_results,
            stats.completions,
        )
    else:
        logger.info("OpenAPPA counters unavailable because every episode was resumed")
    logger.info(
        "separate costs: agent=$%.4f user=$%.4f review=$%.4f NL-judge=$%.4f",
        summary["cost_usd"]["agent"],
        summary["cost_usd"]["user"],
        summary["cost_usd"]["user_review"],
        summary["cost_usd"]["nl_assertion_judge"],
    )
    logger.info("results: %s", output_dir / "results.json")
    return output_dir


def run_bench(
    retrieval_config: str,
    model: str,
    user_model: str,
    judge_model: str,
    review_model: str,
    logdir: str,
    run_name: str | None,
    seed: int,
    max_steps: int,
    max_concurrency: int,
    num_trials: int,
    reasoning_effort: str = "high",
    policy_mode: str = "guarded",
    dry_run: bool = False,
) -> int:
    _execute_bench(
        retrieval_config,
        policy_mode,
        model,
        reasoning_effort,
        user_model,
        judge_model,
        review_model,
        logdir,
        run_name,
        seed,
        max_steps,
        max_concurrency,
        num_trials,
        None,
        True,
        dry_run,
    )
    return 0


def run_pilot(
    retrieval_config: str,
    model: str,
    user_model: str,
    judge_model: str,
    review_model: str,
    logdir: str,
    run_name: str,
    seed: int,
    max_steps: int,
    max_concurrency: int,
    reasoning_effort: str = "high",
    dry_run: bool = False,
) -> int:
    """Run the frozen ten-task slice through guarded, permissive, and stock arms."""
    from appa_taubench.report import build_matched_summary

    directories = {}
    for mode in ("guarded", "permissive", "stock"):
        directories[mode] = _execute_bench(
            retrieval_config,
            mode,
            model,
            reasoning_effort,
            user_model,
            judge_model,
            review_model,
            logdir,
            f"{run_name}-{mode}",
            seed,
            max_steps,
            max_concurrency,
            1,
            PILOT_TASK_IDS,
            False,
            dry_run,
        )
    if not dry_run:
        build_matched_summary(
            directories,
            Path(logdir) / f"{slug(run_name)}-matched-summary.json",
        )
    return 0


def run_chaos_screen(
    retrieval_config: str,
    model: str,
    user_model: str,
    judge_model: str,
    review_model: str,
    logdir: str,
    run_name: str,
    seed: int,
    max_steps: int,
    max_concurrency: int,
    reasoning_effort: str = "high",
    dry_run: bool = False,
) -> int:
    """Screen verification recovery on a compact guarded/permissive slice."""
    from appa_taubench.report import build_matched_summary

    directories = {}
    for mode in ("guarded", "permissive"):
        directories[mode] = _execute_bench(
            retrieval_config,
            mode,
            model,
            reasoning_effort,
            user_model,
            judge_model,
            review_model,
            logdir,
            f"{run_name}-{mode}",
            seed,
            max_steps,
            max_concurrency,
            1,
            CHAOS_SCREEN_TASK_IDS,
            False,
            dry_run,
        )
    if not dry_run:
        build_matched_summary(
            directories,
            Path(logdir) / f"{slug(run_name)}-matched-summary.json",
        )
    return 0
