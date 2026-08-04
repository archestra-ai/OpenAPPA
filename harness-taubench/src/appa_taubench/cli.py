"""Command-line entry point for the OpenAPPA TauBench evaluation."""

import argparse
import logging
import os
from pathlib import Path


def configure_tau2_data_dir(explicit: str | None, parser: argparse.ArgumentParser) -> None:
    if explicit is not None:
        os.environ["TAU2_DATA_DIR"] = explicit
        return
    if "TAU2_DATA_DIR" in os.environ:
        return
    sibling_checkout = Path(__file__).resolve().parents[4] / "taubench" / "data"
    if sibling_checkout.is_dir():
        os.environ["TAU2_DATA_DIR"] = str(sibling_checkout)
        return
    parser.error("TauBench's repository data is required; pass --tau2-data-dir or set TAU2_DATA_DIR")


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="appa-taubench",
        description="Run a minimal OpenAPPA utility evaluation on TauBench",
    )
    parser.add_argument("--domain", choices=["airline"], default="airline")
    parser.add_argument("--defense", choices=["appa", "none", "both"], default="both")
    parser.add_argument("--task-ids", nargs="+", default=["27", "40"])
    parser.add_argument("--model", default="openrouter/openai/gpt-4.1-mini")
    parser.add_argument("--user-model", default=None)
    parser.add_argument("--logdir", default="runs")
    parser.add_argument("--run-name", default=None)
    parser.add_argument("--tau2-data-dir", default=None)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--max-steps", type=int, default=60)
    args = parser.parse_args()

    configure_tau2_data_dir(args.tau2_data_dir, parser)
    defenses = ["none", "appa"] if args.defense == "both" else [args.defense]
    logging.basicConfig(level=logging.INFO, format="%(message)s")

    from appa_taubench.bench import run_bench

    raise SystemExit(
        run_bench(
            domain=args.domain,
            defenses=defenses,
            task_ids=args.task_ids,
            model=args.model,
            user_model=args.user_model or args.model,
            logdir=args.logdir,
            run_name=args.run_name,
            seed=args.seed,
            max_steps=args.max_steps,
        )
    )
