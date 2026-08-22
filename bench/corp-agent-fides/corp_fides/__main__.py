"""``corp-agent-fides``: the corporate assistant on Microsoft Agent Framework,
mediated by FIDES.

    corp-agent-fides "Summarise Alice Chen's HR record"      # FIDES on (default)
    corp-agent-fides --mode unmediated "<injection prompt>"  # the unmediated leak
    corp-agent-fides --chat

Needs an OpenRouter key: ``--api-key``, ``$OPENROUTER_API_KEY``, or a ``.env``
file (see ``.env.example``).
"""

from __future__ import annotations

import argparse
import asyncio
import os
import sys
from pathlib import Path

from .agent import ExecutionMode, build_agent
from .profile import DEFAULT_PROFILE, Profile, ProfileError, load_profile
from .systems import CorpSystemsClient, System, resolve_corpus_root, resolve_sink_root
from .tools import build_tools

_PACKAGE_DIR = Path(__file__).resolve().parent
_CRATE_DIR = _PACKAGE_DIR.parent


def _profile_arg(path: str) -> Profile:
    try:
        return load_profile(path)
    except ProfileError as exc:
        raise argparse.ArgumentTypeError(str(exc)) from exc


def _load_dotenv() -> Path | None:
    """Load ``KEY=VALUE`` lines from ``.env`` without overwriting real env
    vars — crate-local first, then the repo root — mirroring the sibling demo."""
    first: Path | None = None
    for candidate in (_CRATE_DIR / ".env", _CRATE_DIR / ".." / ".." / ".env"):
        candidate = candidate.resolve()
        if not candidate.is_file():
            continue
        for raw in candidate.read_text(encoding="utf-8", errors="replace").splitlines():
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            line = line.removeprefix("export ").strip()
            key, sep, value = line.partition("=")
            key = key.strip()
            if not sep or not key or key in os.environ:
                continue
            os.environ[key] = value.strip().strip('"').strip("'")
        if first is None:
            first = candidate
    return first


def _sink_is_nonempty(sink_root: Path) -> list[str]:
    email_dir = sink_root / System.EMAIL.dir_name
    if not email_dir.is_dir():
        return []
    return sorted(p.name for p in email_dir.iterdir() if p.is_file())


def _response_text(response: object) -> str:
    return getattr(response, "text", None) or str(response)


async def _run_once(built, prompt: str, quiet: bool) -> None:
    response = await built.agent.run(prompt)
    print("\n=== answer ===")
    print(_response_text(response))
    _report(built, quiet)


async def _run_chat(built, quiet: bool) -> None:
    session = built.agent.get_session() if hasattr(built.agent, "get_session") else None
    print("chat mode — type a message, or 'exit' to quit.", file=sys.stderr)
    while True:
        try:
            line = input("\nyou> ").strip()
        except EOFError:
            break
        if not line:
            continue
        if line in ("exit", "quit"):
            break
        response = await built.agent.run(line, session=session)
        print("\n=== answer ===")
        print(_response_text(response))
    _report(built, quiet)


def _report(built, quiet: bool) -> None:
    """Show the observable outcome: what FIDES logged and whether the sink leaked."""
    leaked = _sink_is_nonempty(built.sink_root)
    if built.config is not None and not quiet:
        audit = built.config.get_audit_log()
        print(f"\n=== FIDES audit log ({len(audit)} violation(s)) ===", file=sys.stderr)
        for entry in audit:
            print(
                f"  BLOCKED {entry.get('function', '?')}: "
                f"{entry.get('type', entry.get('subtype', 'violation'))} — "
                f"{entry.get('reason', '')}",
                file=sys.stderr,
            )
    label = "email/ sink"
    if leaked:
        print(f"\n=== {label}: {len(leaked)} message(s) — {', '.join(leaked)} ===", file=sys.stderr)
    else:
        print(f"\n=== {label}: empty (no outbound mail) ===", file=sys.stderr)


def main(argv: list[str] | None = None) -> int:
    dotenv = _load_dotenv()
    parser = argparse.ArgumentParser(
        prog="corp-agent-fides",
        description="The corporate assistant on Microsoft Agent Framework, mediated by FIDES.",
    )
    parser.add_argument("prompt", nargs="?", help="The task for the agent. Omit with --chat for a REPL.")
    parser.add_argument("--chat", action="store_true", help="Interactive REPL instead of a one-shot task.")
    parser.add_argument(
        "--mode",
        type=ExecutionMode,
        choices=list(ExecutionMode),
        default=ExecutionMode.NATIVE_AUTO_HIDE,
        help="Execution mode (default: native-auto-hide).",
    )
    parser.add_argument("--model", default=os.environ.get("FIDES_DEMO_MODEL", "anthropic/claude-sonnet-5"))
    parser.add_argument("--quarantine-model", default=os.environ.get("FIDES_QUARANTINE_MODEL") or None)
    parser.add_argument("--api-key", default=os.environ.get("OPENROUTER_API_KEY"))
    parser.add_argument(
        "--profile",
        type=_profile_arg,
        default=DEFAULT_PROFILE,
        metavar="PATH",
        help="Strict version 1 JSON overrides for FIDES result labels and tool policy metadata.",
    )
    parser.add_argument("--data-root", type=Path, default=None, help="Corpus root (defaults to sibling corp-systems/data).")
    parser.add_argument("--sink-root", type=Path, default=None, help="Where send_email writes (defaults to this demo's data/).")
    parser.add_argument(
        "--server-bin",
        type=Path,
        default=None,
        help="The corp-systems-mcp binary (defaults to the sibling crate's debug build, built on demand).",
    )
    parser.add_argument("--quiet", action="store_true", help="Print only the final answer.")
    args = parser.parse_args(argv)

    if not args.quiet and dotenv is not None:
        print(f"loaded env from {dotenv}", file=sys.stderr)

    api_key = (args.api_key or "").strip().strip('"').strip("'")
    if not api_key:
        parser.error(
            "no OpenRouter API key: pass --api-key, set OPENROUTER_API_KEY, or add it to a .env file "
            "(see .env.example)"
        )

    corpus_root = resolve_corpus_root(args.data_root)
    sink_root = resolve_sink_root(args.sink_root)
    if not args.quiet:
        state = {
            ExecutionMode.UNMEDIATED: "NO DEFENSE (contrast)",
            ExecutionMode.MIDDLEWARE_ONLY: "FIDES ON (middleware-only)",
            ExecutionMode.NATIVE_AUTO_HIDE: "FIDES ON (native auto-hide)",
        }[args.mode]
        print(
            f"corpus {corpus_root} — sink {sink_root} — model {args.model} — {state}",
            file=sys.stderr,
        )

    if not args.chat and not args.prompt:
        parser.error("no task given: pass a prompt argument or use --chat")

    async def _amain() -> None:
        # The shared corp-systems-mcp server stays up for the whole run; the
        # FIDES-labeled tools forward every call through this client.
        async with CorpSystemsClient(corpus_root, sink_root, args.server_bin) as client:
            # Build wrappers only for the tools the live server actually lists —
            # a narrowed --systems / CORP_ENABLED_SYSTEMS surface must not leave
            # the model holding tools the server would refuse.
            available = await client.list_tool_names()
            built = build_agent(
                api_key=api_key,
                model=args.model,
                tools=build_tools(client, available, profile=args.profile),
                sink_root=sink_root,
                mode=args.mode,
                quarantine_model=args.quarantine_model,
                system_prompt_addendum=os.environ.get("APPA_AGENT_PROMPT_ADDENDUM", ""),
            )
            if args.chat:
                await _run_chat(built, args.quiet)
            else:
                await _run_once(built, args.prompt, args.quiet)

    asyncio.run(_amain())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
