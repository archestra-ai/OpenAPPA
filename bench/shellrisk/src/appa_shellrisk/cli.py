"""Command-line entry point for OpenAPPA's ShellRisk-Bench harness."""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

from .dataset import fetch_test, load_jsonl, select_balanced
from .policy import ModelProfile
from .runner import ARMS, run_evaluation

REPO_ROOT = Path(__file__).resolve().parents[4]
BENCH_DIR = REPO_ROOT / "bench" / "shellrisk"
DEFAULT_CACHE = REPO_ROOT / "data" / "shellrisk-v0.1-test.jsonl"


def positive(value: str) -> int:
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be at least one")
    return parsed


def _add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--arm", action="append", choices=ARMS, dest="arms", help="arm to run; repeatable")
    parser.add_argument("--provider", choices=["anthropic", "openai", "gemini", "ollama"], default="openai")
    parser.add_argument("--model", default="openai/gpt-5.6-luna")
    parser.add_argument("--url", default="https://openrouter.ai/api/v1")
    parser.add_argument("--token-env", default="OPENROUTER_API_KEY")
    parser.add_argument("--timeout-ms", type=positive, default=120_000)
    parser.add_argument("--max-concurrent", type=positive, default=4)
    parser.add_argument("--jobs", type=positive, default=4)
    parser.add_argument("--appa-bin", type=Path, default=REPO_ROOT / "target" / "debug" / "appa")
    parser.add_argument("--dataset", type=Path, default=None, help="prepared ShellRisk JSONL instead of the Hub split")
    parser.add_argument("--dataset-cache", type=Path, default=DEFAULT_CACHE)
    parser.add_argument("--output", type=Path, default=None)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="appa-shellrisk", description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    preflight = commands.add_parser("preflight", help="check the pinned dataset, binary, and credential presence")
    _add_common(preflight)

    smoke = commands.add_parser("smoke", help="run a small balanced subset (six commands by default)")
    _add_common(smoke)
    smoke.add_argument("--limit", type=positive, default=6)

    run = commands.add_parser("run", help="run an explicit subset or the complete test split")
    _add_common(run)
    selection = run.add_mutually_exclusive_group(required=True)
    selection.add_argument("--limit", type=positive, help="balanced number of test commands to run")
    selection.add_argument("--full", action="store_true", help="explicitly run all 4,194 test commands")
    return parser


def _profile(args: argparse.Namespace) -> ModelProfile:
    return ModelProfile(
        provider=args.provider,
        model=args.model,
        url=args.url or None,
        token_env=args.token_env or None,
        timeout_ms=args.timeout_ms,
        max_concurrent=args.max_concurrent,
    )


def _rows(args: argparse.Namespace):
    return load_jsonl(args.dataset) if args.dataset else fetch_test(args.dataset_cache)


def _preflight(args: argparse.Namespace) -> None:
    rows = _rows(args)
    arms = args.arms or list(ARMS)
    if any(arm in {"annotator", "authority"} for arm in arms) and not args.appa_bin.is_file():
        raise ValueError(f"appa binary not found at {args.appa_bin}; run `cargo build --package appa`")
    profile = _profile(args)
    if profile.provider != "ollama":
        import os

        if not profile.token_env or not os.environ.get(profile.token_env):
            raise ValueError(f"the model token variable {profile.token_env!r} is not set")
    if "bare" in arms and profile.provider != "openai":
        raise ValueError("the bare arm currently requires --provider openai")
    print(
        json.dumps(
            {
                "dataset_rows": len(rows),
                "arms": arms,
                "provider": profile.provider,
                "model": profile.model,
                "appa_bin": str(args.appa_bin),
            },
            indent=2,
        )
    )


def _selection_limit(args: argparse.Namespace, total: int) -> int:
    if total < 1:
        raise ValueError("the selected dataset contains no commands")
    if args.command == "run" and args.full:
        return total
    if args.limit >= total:
        raise ValueError(f"--limit would select the complete {total}-command dataset; use --full explicitly")
    return args.limit


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "preflight":
            _preflight(args)
            return 0
        rows = _rows(args)
        limit = _selection_limit(args, len(rows))
        selected = select_balanced(rows, limit)
        arms = args.arms or list(ARMS)
        output = args.output or BENCH_DIR / "runs" / time.strftime("%Y%m%d-%H%M%S")
        summaries = run_evaluation(
            repo=REPO_ROOT,
            rows=selected,
            arms=arms,
            profile=_profile(args),
            appa_bin=args.appa_bin,
            run_dir=output,
            jobs=args.jobs,
        )
        print(json.dumps(summaries, indent=2, sort_keys=True))
        print(f"records: {output}", file=sys.stderr)
        return 0
    except (OSError, RuntimeError, ValueError) as error:
        parser.exit(1, f"error: {error}\n")
