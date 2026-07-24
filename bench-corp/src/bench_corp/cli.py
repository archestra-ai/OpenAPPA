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

from .agents import DEFAULT_MODEL, REPO_ROOT, AGENTS, Agent, build_binaries
from .report import print_scenario_table, print_table, summarize, write_summary
from .runner import EpisodeResult, run_episode
from .scenario import Scenario, ScenarioError, discover_scenarios

BENCH_DIR = Path(__file__).resolve().parents[2]
SCENARIOS_DIR = BENCH_DIR / "scenarios"


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
        )
        for index, (agent, scenario, rep) in enumerate(episodes, start=1)
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="bench-corp", description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    run_parser = sub.add_parser("run", help="Run the grid and print the summary table.")
    run_parser.add_argument(
        "--agent",
        action="append",
        choices=sorted(AGENTS),
        help="Agent to run (repeatable). Default: all of them.",
    )
    run_parser.add_argument(
        "--scenario", action="append", help="Scenario name under scenarios/ (repeatable). Default: all."
    )
    run_parser.add_argument("--reps", type=int, default=1, help="Repetitions per cell (default 1).")
    run_parser.add_argument("--model", default=DEFAULT_MODEL, help=f"Shared OpenRouter model (default {DEFAULT_MODEL}).")
    run_parser.add_argument("--timeout", type=float, default=300.0, help="Per-episode timeout in seconds (default 300).")
    run_parser.add_argument(
        "-j", "--jobs", type=int, default=-1, help="Concurrent episodes (default -1: all CPUs; 1: sequential)."
    )
    run_parser.add_argument("--runs-dir", type=Path, default=BENCH_DIR / "runs", help="Where run records land.")
    run_parser.add_argument("--skip-build", action="store_true", help="Skip the up-front cargo builds.")
    args = parser.parse_args(argv)

    try:
        scenarios = discover_scenarios(SCENARIOS_DIR, args.scenario)
    except ScenarioError as error:
        parser.error(str(error))
    agents = [AGENTS[name] for name in (args.agent or sorted(AGENTS))]
    if args.reps < 1:
        parser.error("--reps must be at least 1")
    if args.jobs == 0:
        parser.error("--jobs must not be 0")

    if not args.skip_build:
        build_binaries(agents)

    run_id = time.strftime("%Y%m%d-%H%M%S")
    run_dir = args.runs_dir / run_id
    run_dir.mkdir(parents=True)
    (run_dir / "config.json").write_text(
        json.dumps(
            {
                "model": args.model,
                "reps": args.reps,
                "timeout_s": args.timeout,
                "jobs": args.jobs,
                "agents": [s.name for s in agents],
                "scenarios": [s.name for s in scenarios],
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
