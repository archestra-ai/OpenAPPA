"""`appa-sanitizer-bench`: list the corpus, preflight without a model, run arms, summarize."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from appa_bench_publish import add_publish_parser, publish_from_args

from . import corpus, runner, scoring

ARMS = ("identity", "claude-code", "llm")


def _git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=runner.REPO_ROOT, capture_output=True, text=True, check=True).stdout


def _provenance() -> dict:
    return {"git_sha": _git("rev-parse", "HEAD").strip(), "git_dirty": bool(_git("status", "--porcelain").strip())}


def _backend(args: argparse.Namespace, arm: str) -> runner.Backend:
    match arm:
        case "identity":
            return runner.Backend(builtin="identity", max_body_bytes=args.max_body_bytes)
        case "claude-code":
            command = args.claude_command or runner.resolve_claude_command()
            return runner.Backend(
                builtin="claude-code",
                model=args.claude_model,
                claude_dir=str(Path(command).parent) if command else None,
                consult_timeout_ms=args.consult_timeout_ms,
                max_concurrent=args.concurrency,
                max_body_bytes=args.max_body_bytes,
            )
        case "llm":
            return runner.Backend(
                builtin="llm",
                provider=args.llm_provider,
                model=args.llm_model,
                url=args.llm_url,
                consult_timeout_ms=args.consult_timeout_ms,
                max_concurrent=args.concurrency,
                max_body_bytes=args.max_body_bytes,
            )
    raise ValueError(arm)


def _selected(args: argparse.Namespace) -> list[corpus.Case]:
    """The cases the filters select. An unknown sanitizer or an empty selection is an error: a
    run that asks nothing must not read as a completed benchmark."""
    registered = corpus.sanitizers()
    unknown = sorted(set(args.sanitizer or ()) - set(registered))
    if unknown:
        raise ValueError(f"unknown sanitizer {unknown}; the bench policy registers {sorted(registered)}")
    cases = corpus.load()
    if args.sanitizer:
        cases = [case for case in cases if case.sanitizer in args.sanitizer]
    if args.kind:
        cases = [case for case in cases if case.kind in args.kind]
    if not cases:
        raise ValueError("the filters select no case")
    return cases


def _missing_prerequisites(args: argparse.Namespace, arms: list[str]) -> list[str]:
    """What a selected arm needs and does not have, checked before any arm runs."""
    missing = []
    if "llm" in arms and not os.environ.get(args.llm_token_env):
        missing.append(f"the llm arm reads its key from {args.llm_token_env}, which is not set")
    if "claude-code" in arms and _backend(args, "claude-code").claude_dir is None:
        missing.append("the claude-code arm found no claude executable; pass --claude-command")
    return missing


def _run_arm(args, arm: str, cases: list[corpus.Case], output: Path) -> dict:
    backend = _backend(args, arm)
    token = os.environ.get(args.llm_token_env) if arm == "llm" else None
    started = time.time()
    rows = runner.ask(cases, backend, output / arm, repeat=args.repeat, concurrency=args.concurrency, token=token)
    problems = scoring.incomplete(cases, rows, args.repeat)
    if problems:
        sys.exit(f"the {arm} arm's answers are not one per case and repeat: {'; '.join(problems)}")
    summary = scoring.summarize(cases, rows, corpus.sanitizers())
    summary["arm"] = arm
    summary["backend"] = backend.describe()
    summary["wall_s"] = round(time.time() - started, 1)
    (output / arm / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    return summary


def _headline(summary: dict) -> dict:
    """The per-sanitizer numbers without the per-row detail."""
    return {
        "arm": summary["arm"],
        "backend": summary["backend"],
        "unresolved": len(summary["unresolved"]),
        "sanitizers": {
            name: {key: value for key, value in entry.items() if key != "by_size"}
            for name, entry in summary["sanitizers"].items()
        },
        "by_size": {name: entry["by_size"] for name, entry in summary["sanitizers"].items()},
    }


def _complete(run_dir: Path) -> bool:
    """Whether every arm of the run finished with a full, resolved answer set."""
    try:
        return json.loads((run_dir / "summary.json").read_text()).get("complete") is True
    except (OSError, json.JSONDecodeError):
        return False


def main() -> None:
    parser = argparse.ArgumentParser(description="LLM sanitizers asked through the production consult path")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("list", help="print every case without asking anything")
    preflight = commands.add_parser("preflight", help="run the identity arm: no model, checks the whole path")
    run = commands.add_parser("run", help="ask the selected arms and write a run directory")
    for sub in (preflight, run):
        sub.add_argument("--output", type=Path, required=sub is run)
        sub.add_argument("--sanitizer", action="append")
        sub.add_argument("--kind", action="append", choices=corpus.KINDS)
        sub.add_argument("--repeat", type=int, default=1)
        sub.add_argument("--concurrency", type=int, default=4)
        sub.add_argument("--max-body-bytes", type=int, default=runner.SHIPPED_MAX_BODY_BYTES)
        sub.add_argument("--skip-build", action="store_true")
    run.add_argument("--arm", action="append", choices=ARMS, help="default: identity and claude-code")
    run.add_argument("--claude-model", default="sonnet", help="the shipped default alias")
    run.add_argument("--claude-command")
    run.add_argument("--llm-provider", default="anthropic", choices=["anthropic", "openai", "gemini", "ollama"])
    run.add_argument("--llm-model", default="claude-haiku-4-5")
    run.add_argument("--llm-url")
    run.add_argument("--llm-token-env", default="ANTHROPIC_API_KEY")
    run.add_argument(
        "--consult-timeout-ms",
        type=int,
        help="the model table's timeout_ms; unset keeps the runtime default (60000)",
    )
    add_publish_parser(commands)
    args = parser.parse_args()

    if args.command == "publish":
        if not _complete(args.run_dir):
            parser.error(f"{args.run_dir} is not a complete run: its summary.json does not record complete = true")
        publish_from_args(args, "sanitizer")
        return

    if args.command == "list":
        for case in corpus.load():
            print(
                json.dumps(
                    {
                        "id": case.id,
                        "sanitizer": case.sanitizer,
                        "kind": case.kind,
                        "family": case.family,
                        "bytes": len(case.text().encode()),
                    }
                )
            )
        return
    if min(args.repeat, args.concurrency) <= 0:
        parser.error("repeat and concurrency must be positive")
    try:
        cases = _selected(args)
    except ValueError as error:
        parser.error(str(error))
    arms = ["identity"] if args.command == "preflight" else args.arm or ["identity", "claude-code"]
    if args.command == "run":
        if args.output.exists():
            parser.error(f"output exists: {args.output}; runs are never overwritten")
        missing = _missing_prerequisites(args, arms)
        if missing:
            parser.error("; ".join(missing))
    if not args.skip_build:
        runner.build_appa()

    if args.command == "preflight":
        output = args.output or Path("/tmp") / f"appa-sanitizer-preflight-{os.getpid()}"
        summary = _run_arm(args, "identity", cases, output)
        print(json.dumps(_headline(summary), indent=2))
        if summary["unresolved"] or any(e["withheld"] for e in summary["sanitizers"].values() if e):
            sys.exit("preflight: the identity arm should derive every value")
        return

    args.output.mkdir(parents=True)
    config = {
        **_provenance(),
        "arms": arms,
        "repeat": args.repeat,
        "concurrency": args.concurrency,
        "corpus_sha256": corpus.digest(cases),
        "cases": len(cases),
        "policy_sha256": hashlib.sha256(corpus.POLICY.read_bytes()).hexdigest(),
        "batteries_sha256": {
            include: hashlib.sha256(path.read_bytes()).hexdigest() for include, path in corpus.included().items()
        },
    }
    (args.output / "run-config.json").write_text(json.dumps(config, indent=2) + "\n")
    summaries = [_headline(_run_arm(args, arm, cases, args.output)) for arm in arms]
    complete = not any(summary["unresolved"] for summary in summaries)
    # Written only once every arm returned a full answer set: `publish` requires it.
    result = {"config": config, "complete": complete, "arms": summaries}
    (args.output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    if not complete:
        sys.exit("some values were unresolved: a harness error, see summary.json")


if __name__ == "__main__":
    main()
