"""``bench-corp run``: drive the agent × scenario × rep grid and score it.

Reproducibility: ``config.json`` in every run dir records the model, reps,
jobs, agent and scenario lists, git SHA, and whether the worktree was dirty.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

from joblib import Parallel, delayed

from . import AGENT_PROMPT_PROFILES, CHAOS_SCREEN_SCENARIOS
from .agents import DEFAULT_MODEL, REPO_ROOT, AGENTS, Agent, build_binaries
from .report import print_scenario_table, print_table, summarize, write_summary
from .runner import EpisodeResult, run_episode
from .scenario import Scenario, ScenarioError, discover_scenarios

BENCH_DIR = Path(__file__).resolve().parents[2]
SCENARIOS_DIR = BENCH_DIR / "scenarios"
AGENT_ALIASES = {"fides": "fides-native"}


def resolve_agent_names(names: list[str] | None) -> list[str]:
    """Resolve compatibility names without adding duplicate benchmark arms."""
    requested = sorted(AGENTS) if names is None else names
    resolved: list[str] = []
    for name in requested:
        canonical = AGENT_ALIASES.get(name, name)
        if canonical not in resolved:
            resolved.append(canonical)
    return resolved


def _git_state() -> dict:
    def run(*args: str) -> str:
        return subprocess.run(
            ["git", *args], cwd=REPO_ROOT, capture_output=True, text=True, check=False
        ).stdout.strip()

    return {"git_sha": run("rev-parse", "HEAD"), "git_dirty": bool(run("status", "--porcelain"))}


def _execute_episode(
    index: int,
    total: int,
    agent: Agent,
    scenario: Scenario,
    rep: int,
    *,
    model: str,
    run_dir: Path,
    timeout_s: float,
    agent_prompt_profile: str = "standard",
) -> EpisodeResult:
    label = f"{agent.name} / {scenario.name} / rep{rep}"
    print(f"[{index}/{total}] starting {label}", file=sys.stderr)
    result = run_episode(
        agent,
        scenario,
        rep,
        model=model,
        episode_dir=run_dir / agent.name / scenario.name / f"rep{rep}",
        timeout_s=timeout_s,
        agent_prompt_profile=agent_prompt_profile,
    )
    status = "error " + result.error if result.error else "ok"
    print(
        f"[{index}/{total}] finished {label}: {status}; utility={result.utility} "
        f"security={result.security} emails={result.emails} ({result.duration_s}s)",
        file=sys.stderr,
    )
    return result


def _run_grid(
    agents: list[Agent],
    scenarios: list[Scenario],
    *,
    reps: int,
    model: str,
    run_dir: Path,
    timeout_s: float,
    jobs: int,
    agent_prompt_profile: str = "standard",
) -> list[EpisodeResult]:
    episodes = [
        (agent, scenario, rep)
        for agent in agents
        for scenario in scenarios
        for rep in range(1, reps + 1)
    ]
    total = len(episodes)
    return Parallel(n_jobs=jobs, prefer="threads")(
        delayed(_execute_episode)(
            index,
            total,
            agent,
            scenario,
            rep,
            model=model,
            run_dir=run_dir,
            timeout_s=timeout_s,
            agent_prompt_profile=agent_prompt_profile,
        )
        for index, (agent, scenario, rep) in enumerate(episodes, start=1)
    )


def _add_execution_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--reps", type=int, default=1, help="Repetitions per cell (default 1).")
    parser.add_argument("--model", default=DEFAULT_MODEL, help=f"Shared OpenRouter model (default {DEFAULT_MODEL}).")
    parser.add_argument("--timeout", type=float, default=300.0, help="Per-episode timeout in seconds (default 300).")
    parser.add_argument(
        "-j", "--jobs", type=int, default=-1, help="Concurrent episodes (default -1: all CPUs; 1: sequential)."
    )
    parser.add_argument("--runs-dir", type=Path, default=BENCH_DIR / "runs", help="Where run records land.")
    parser.add_argument("--skip-build", action="store_true", help="Skip the up-front cargo builds.")
    parser.add_argument(
        "--agent-prompt-profile",
        choices=AGENT_PROMPT_PROFILES,
        default="standard",
        help="Recorded system-prompt addendum (default standard: no addendum).",
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="bench-corp", description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    run_parser = sub.add_parser("run", help="Run the grid and print the summary table.")
    run_parser.add_argument(
        "--agent",
        action="append",
        choices=sorted((*AGENTS, *AGENT_ALIASES)),
        help="Agent to run (repeatable). Default: all of them.",
    )
    run_parser.add_argument(
        "--scenario", action="append", help="Scenario name under scenarios/ (repeatable). Default: all."
    )
    _add_execution_arguments(run_parser)
    screen_parser = sub.add_parser(
        "chaos-screen",
        help="Run the matched APPA guarded/open mechanism-probe screen.",
    )
    _add_execution_arguments(screen_parser)
    args = parser.parse_args(argv)

    selected_scenarios = (
        list(CHAOS_SCREEN_SCENARIOS) if args.command == "chaos-screen" else args.scenario
    )
    try:
        scenarios = discover_scenarios(SCENARIOS_DIR, selected_scenarios)
    except ScenarioError as error:
        parser.error(str(error))
    selected_agents = (
        ["appa", "appa-open"] if args.command == "chaos-screen" else resolve_agent_names(args.agent)
    )
    agents = [AGENTS[name] for name in selected_agents]
    if args.reps < 1:
        parser.error("--reps must be at least 1")
    if args.jobs == 0:
        parser.error("--jobs must not be 0")

    if not args.skip_build:
        build_binaries(agents)

    # One run per model, launched together, starts inside the same second, so a
    # timestamp alone is not a run id: take the first free suffix rather than
    # letting the losers die on FileExistsError.
    stamp = time.strftime("%Y%m%d-%H%M%S")
    attempt = 1
    while True:
        run_id = stamp if attempt == 1 else f"{stamp}-{attempt}"
        run_dir = args.runs_dir / run_id
        try:
            run_dir.mkdir(parents=True)
            break
        except FileExistsError:
            attempt += 1
    (run_dir / "config.json").write_text(
        json.dumps(
            {
                "model": args.model,
                "reps": args.reps,
                "timeout_s": args.timeout,
                "jobs": args.jobs,
                "agents": [s.name for s in agents],
                "scenarios": [s.name for s in scenarios],
                "agent_prompt_profile": args.agent_prompt_profile,
                **_git_state(),
            },
            indent=2,
        )
        + "\n"
    )

    results = _run_grid(
        agents,
        scenarios,
        reps=args.reps,
        model=args.model,
        run_dir=run_dir,
        timeout_s=args.timeout,
        jobs=args.jobs,
        agent_prompt_profile=args.agent_prompt_profile,
    )

    summaries = summarize(results)
    write_summary(run_dir, summaries, results)
    print(f"\nrun {run_id} — model {args.model}, {args.reps} rep(s)\n")
    print_table(summaries)
    print_scenario_table(results)
    print(f"\nfull records: {run_dir}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
