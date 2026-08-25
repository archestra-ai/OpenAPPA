"""Command-line entry point for the complete AgentThreatBench harness."""

import argparse

from appa_agentthreatbench.runner import SMOKE_SAMPLE_IDS, preflight, run_complete
from appa_agentthreatbench.tasks import AGENT_PROMPT_PROFILES, ARMS, TASK_TYPES, complete_dataset

DEFAULT_MODEL = "openrouter/openai/gpt-5.6-luna"
REASONING_EFFORTS = ("none", "minimal", "low", "medium", "high", "xhigh", "max")
TASK_TYPE_CHOICES = ("all", *TASK_TYPES)
ARM_CHOICES = ("all", "openappa", *ARMS)


def integer_at_least(minimum: int):
    def parse(value: str) -> int:
        parsed = int(value)
        if parsed < minimum:
            raise argparse.ArgumentTypeError(f"must be at least {minimum}")
        return parsed

    return parse


def resolve_samples(command: str, task_type: str = "all", arms: str = "all") -> list[str] | None:
    if command == "smoke":
        return list(SMOKE_SAMPLE_IDS)
    if task_type == "all" and arms == "all":
        return None
    dataset = complete_dataset()
    selected_arms = ARMS if arms == "all" else (("stock", "permissive", "guarded") if arms == "openappa" else (arms,))
    selected_types = TASK_TYPES if task_type == "all" else (task_type,)
    ids = [
        str(sample.id)
        for sample in dataset
        if sample.metadata.get("appa_arm") in selected_arms and sample.metadata.get("task_type") in selected_types
    ]
    return ids


def add_execution_arguments(parser: argparse.ArgumentParser, default_concurrency: int) -> None:
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--reasoning-effort", choices=REASONING_EFFORTS, default="high")
    parser.add_argument("--max-concurrency", type=integer_at_least(1), default=default_concurrency)
    parser.add_argument("--seed", type=int, default=300)
    parser.add_argument("--logdir", default="runs")
    parser.add_argument("--run-name", default=None)
    parser.add_argument("--agent-prompt-profile", choices=AGENT_PROMPT_PROFILES, default="standard")
    parser.add_argument("--dry-run", action="store_true")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="appa-agentthreatbench",
        description="Run AgentThreatBench tasks through stock, OpenAPPA, and FIDES arms",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    preflight_parser = commands.add_parser("preflight", help="validate pins, policies, inventory, and credentials")
    preflight_parser.add_argument("--model", default=DEFAULT_MODEL)

    smoke_parser = commands.add_parser("smoke", help="run a manifested 20-sample lifecycle smoke test")
    add_execution_arguments(smoke_parser, default_concurrency=10)

    run_parser = commands.add_parser("run", help="run upstream and control samples")
    add_execution_arguments(run_parser, default_concurrency=50)
    run_parser.add_argument("--task-type", choices=TASK_TYPE_CHOICES, default="all")
    run_parser.add_argument("--arms", choices=ARM_CHOICES, default="all")

    render_parser = commands.add_parser("render", help="render benchmark trajectories into readable Markdown")
    render_parser.add_argument("--run-dir", "-r", required=True, help="run directory or eval log")
    render_parser.add_argument("--compare-run-dir", "-c", default=None, help="second run directory to compare")
    render_parser.add_argument("--output-file", "-o", default=None, help="output markdown file path")
    render_parser.add_argument(
        "--task-type",
        default="all",
        choices=["all", *TASK_TYPES],
        help="filter tasks by type",
    )
    return parser


def main() -> None:
    parser = build_parser()
    args = parser.parse_args()
    try:
        if args.command == "preflight":
            preflight(args.model)
            return
        if args.command == "render":
            from pathlib import Path

            from appa_agentthreatbench.render import (
                default_report_path,
                render_comparison_markdown,
                resolve_eval_log_path,
            )

            run_path = Path(args.run_dir)
            eval_file = resolve_eval_log_path(run_path)

            comp_file = None
            if args.compare_run_dir:
                comp_file = resolve_eval_log_path(Path(args.compare_run_dir))

            out_file = Path(args.output_file) if args.output_file else default_report_path(run_path, eval_file)
            render_comparison_markdown(
                eval_log_path=eval_file,
                compare_log_path=comp_file,
                output_file=out_file,
                task_type_filter=args.task_type,
            )
            print(f"Trajectory report rendered: {out_file}")
            return

        sample_ids = resolve_samples(
            args.command,
            task_type=getattr(args, "task_type", "all"),
            arms=getattr(args, "arms", "all"),
        )
        run_complete(
            model=args.model,
            reasoning_effort=args.reasoning_effort,
            max_concurrency=args.max_concurrency,
            seed=args.seed,
            logdir=args.logdir,
            run_name=args.run_name,
            dry_run=args.dry_run,
            agent_prompt_profile=args.agent_prompt_profile,
            sample_ids=sample_ids,
        )
    except (RuntimeError, ValueError) as error:
        parser.exit(1, f"error: {error}\n")
