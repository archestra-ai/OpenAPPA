"""Command-line entry point for the OpenAPPA TauBench evaluation."""

import argparse
import logging
import os
from pathlib import Path

from appa_taubench import SUPPORTED_RETRIEVAL_CONFIGS
from appa_taubench.policies import POLICY_MODES

DEFAULT_MODEL = "openrouter/openai/gpt-5.2"
DEFAULT_USER_MODEL = "openrouter/openai/gpt-5.2"
DEFAULT_JUDGE_MODEL = "openrouter/openai/gpt-4.1"


def configure_tau2_data_dir(explicit: str | None, parser: argparse.ArgumentParser) -> None:
    if explicit is not None:
        os.environ["TAU2_DATA_DIR"] = explicit
        return
    if "TAU2_DATA_DIR" in os.environ:
        return
    managed_checkout = Path(__file__).resolve().parents[2] / ".tau2-bench" / "data"
    if managed_checkout.is_dir():
        os.environ["TAU2_DATA_DIR"] = str(managed_checkout)
        return
    parser.error(
        "TauBench's repository data is required; run ./setup-taubench.sh, pass --tau2-data-dir, or set TAU2_DATA_DIR"
    )


def integer_at_least(minimum: int):
    def parse(value: str) -> int:
        parsed = int(value)
        if parsed < minimum:
            raise argparse.ArgumentTypeError(f"must be at least {minimum}")
        return parsed

    return parse


def add_data_argument(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--tau2-data-dir", default=None)


def add_model_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--retrieval-config", choices=SUPPORTED_RETRIEVAL_CONFIGS, default="alltools-qwen")
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--user-model", default=DEFAULT_USER_MODEL)
    parser.add_argument("--judge-model", default=DEFAULT_JUDGE_MODEL)
    parser.add_argument("--review-model", default=None)


def add_execution_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--logdir", default="runs")
    parser.add_argument("--run-name", default=None)
    parser.add_argument("--seed", type=int, default=300)
    parser.add_argument("--max-steps", type=integer_at_least(1), default=200)
    parser.add_argument("--max-concurrency", type=integer_at_least(1), default=3)
    parser.add_argument("--dry-run", action="store_true", help="validate and print the plan without invoking Tau")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="appa-taubench",
        description="Run and submit OpenAPPA on Tau Knowledge",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    preflight_parser = commands.add_parser("preflight", help="check Knowledge dependencies without API calls")
    add_data_argument(preflight_parser)
    add_model_arguments(preflight_parser)

    run_parser = commands.add_parser("run", help="run the complete Knowledge base split")
    add_data_argument(run_parser)
    add_model_arguments(run_parser)
    add_execution_arguments(run_parser)
    run_parser.add_argument("--num-trials", type=integer_at_least(4), default=4)
    run_parser.add_argument("--policy-mode", choices=POLICY_MODES, default="guarded")

    pilot_parser = commands.add_parser("pilot", help="run matched ten-task guarded, permissive, and stock arms")
    add_data_argument(pilot_parser)
    add_model_arguments(pilot_parser)
    add_execution_arguments(pilot_parser)
    pilot_parser.set_defaults(run_name="tau-knowledge-pilot")

    submit_parser = commands.add_parser("submit", help="prepare and validate a completed custom submission")
    add_data_argument(submit_parser)
    submit_parser.add_argument("run_dir")
    submit_parser.add_argument("--output", required=True)
    return parser


def main() -> None:
    parser = build_parser()
    args = parser.parse_args()

    configure_tau2_data_dir(args.tau2_data_dir, parser)
    logging.basicConfig(level=logging.INFO, format="%(message)s")

    if args.command == "submit":
        from appa_taubench.submission import prepare_custom_submission

        submission_dir = prepare_custom_submission(args.run_dir, args.output)
        print(f"submission: {submission_dir}")
        return

    from appa_taubench.bench import preflight, run_bench, run_pilot

    user_model = args.user_model
    review_model = args.review_model or args.judge_model
    if args.command == "preflight":
        try:
            preflight(
                args.retrieval_config,
                args.model,
                user_model,
                args.judge_model,
                review_model,
            )
        except (RuntimeError, ValueError) as error:
            parser.exit(1, f"error: {error}\n")
        return
    if args.command == "pilot":
        raise SystemExit(
            run_pilot(
                retrieval_config=args.retrieval_config,
                model=args.model,
                user_model=user_model,
                judge_model=args.judge_model,
                review_model=review_model,
                logdir=args.logdir,
                run_name=args.run_name,
                seed=args.seed,
                max_steps=args.max_steps,
                max_concurrency=args.max_concurrency,
                dry_run=args.dry_run,
            )
        )
    raise SystemExit(
        run_bench(
            retrieval_config=args.retrieval_config,
            model=args.model,
            user_model=user_model,
            judge_model=args.judge_model,
            review_model=review_model,
            logdir=args.logdir,
            run_name=args.run_name,
            seed=args.seed,
            max_steps=args.max_steps,
            max_concurrency=args.max_concurrency,
            num_trials=args.num_trials,
            policy_mode=args.policy_mode,
            dry_run=args.dry_run,
        )
    )
