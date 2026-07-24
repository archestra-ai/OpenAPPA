"""Command-line entry point for the APPA AgentDojo evaluation."""

import argparse
import logging


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="appa-dojo",
        description="Run the current OpenAPPA engine against AgentDojo",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    bench = subparsers.add_parser("bench")
    bench.add_argument("--suite", default="workspace")
    bench.add_argument("--benchmark-version", default="v1.2.2")
    bench.add_argument("--model", default="openai/gpt-5.6-luna")
    bench.add_argument("--attack", default="tool_knowledge")
    bench.add_argument(
        "--defense",
        choices=["appa", "appa-open", "appa-practical", "appa-complete", "none"],
        default="appa",
    )
    bench.add_argument("--user-tasks", nargs="*", default=None)
    bench.add_argument("--injection-tasks", nargs="*", default=None)
    bench.add_argument("--logdir", default="runs")
    bench.add_argument("--skip-clean-utility", action="store_true")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(message)s")
    if args.command == "bench":
        from appa_dojo.bench import run_bench

        raise SystemExit(
            run_bench(
                suite_name=args.suite,
                benchmark_version=args.benchmark_version,
                model=args.model,
                attack_name=args.attack,
                defense=args.defense,
                user_tasks=args.user_tasks,
                injection_tasks=args.injection_tasks,
                logdir=args.logdir,
                skip_clean_utility=args.skip_clean_utility,
            )
        )
