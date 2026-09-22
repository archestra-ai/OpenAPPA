"""Mine tool calls from local Claude Code transcripts into `appa runtime annotate` input.

Calls keep their complete arguments: an Annotator sees the whole call, and the tail of a
long command can decide its label. Known secret shapes are redacted. The output holds
real commands, paths, and hostnames: review it before it leaves the machine.
"""

import argparse
import hashlib
import json
import logging
import random
import re
import sys
from collections import defaultdict
from collections.abc import Iterator
from pathlib import Path

logger = logging.getLogger(__name__)

# `appa runtime annotate` refuses a consult above the deployment's `max_body_bytes`.
MAX_CALL_BYTES = 48 * 1024

SECRET_PATTERNS = [
    (re.compile(r"sk-[A-Za-z0-9_\-]{16,}"), "<REDACTED_KEY>"),
    (re.compile(r"gh[pousr]_[A-Za-z0-9]{16,}"), "<REDACTED_KEY>"),
    (re.compile(r"xox[baprs]-[A-Za-z0-9\-]{10,}"), "<REDACTED_KEY>"),
    (re.compile(r"AKIA[0-9A-Z]{16}"), "<REDACTED_KEY>"),
    (re.compile(r"(?i)(authorization:\s*bearer\s+)[A-Za-z0-9._\-]{12,}"), r"\1<REDACTED_KEY>"),
    (
        re.compile(r"(?i)\b(api[_-]?key|secret|token|password)\b(\s*[:=]\s*)[\"']?[A-Za-z0-9._\-]{12,}[\"']?"),
        r"\1\2<REDACTED>",
    ),
    (re.compile(r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}"), "<REDACTED_JWT>"),
]


def scrub(text: str) -> str:
    for pattern, replacement in SECRET_PATTERNS:
        text = pattern.sub(replacement, text)
    return text


def tool_uses(transcript: Path) -> Iterator[tuple[str, dict]]:
    with transcript.open(encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if '"tool_use"' not in line:
                continue
            try:
                message = json.loads(line).get("message")
            except json.JSONDecodeError:
                continue
            content = message.get("content") if isinstance(message, dict) else None
            for block in content if isinstance(content, list) else []:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    name, arguments = block.get("name"), block.get("input")
                    if isinstance(name, str) and isinstance(arguments, dict) and arguments:
                        yield name, arguments


def collect(transcripts: list[Path], tool: re.Pattern, text: re.Pattern | None, known: set[str]) -> dict[str, list]:
    """Distinct calls by tool name, skipping ids already in `known`."""
    calls: dict[str, list[dict]] = defaultdict(list)
    seen = set(known)
    for transcript in transcripts:
        try:
            for name, arguments in tool_uses(transcript):
                if not tool.fullmatch(name):
                    continue
                payload = scrub(json.dumps({"tool": name, "arguments": arguments}, ensure_ascii=False, sort_keys=True))
                if len(payload.encode()) > MAX_CALL_BYTES or (text and not text.search(payload)):
                    continue
                digest = hashlib.sha256(payload.encode()).hexdigest()[:16]
                if digest in seen:
                    continue
                seen.add(digest)
                calls[name].append({"id": digest, **json.loads(payload), "source_project": transcript.parent.name})
        except OSError:
            logger.exception("could not read %s", transcript)
    return calls


def sample(calls: dict[str, list[dict]], count: int, rng: random.Random) -> list[dict]:
    """Round-robin over tool names, so one common tool does not crowd out the rest."""
    for batch in calls.values():
        rng.shuffle(batch)
    names = sorted(calls, key=lambda name: (-len(calls[name]), name))
    picked: list[dict] = []
    depth = 0
    while len(picked) < count and any(depth < len(calls[name]) for name in names):
        for name in names:
            if depth < len(calls[name]) and len(picked) < count:
                picked.append(calls[name][depth])
        depth += 1
    return picked


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(message)s", stream=sys.stderr)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--transcripts", type=Path, default=Path.home() / ".claude" / "projects")
    parser.add_argument("--tool", default=".*", help="regex a tool name must match in full")
    parser.add_argument("--match", help="regex the call's JSON must contain, for mining one kind of call")
    parser.add_argument("--exclude", type=Path, action="append", default=[], help="calls files whose ids to skip")
    parser.add_argument("--count", type=int, default=100)
    parser.add_argument("--seed", type=int, default=1337)
    args = parser.parse_args()

    known = set()
    for path in args.exclude:
        with path.open(encoding="utf-8") as handle:
            known.update(json.loads(line)["id"] for line in handle if line.strip())
    transcripts = sorted(args.transcripts.rglob("*.jsonl"))
    calls = collect(transcripts, re.compile(args.tool), re.compile(args.match) if args.match else None, known)
    logger.info("%d distinct calls over %d tools", sum(map(len, calls.values())), len(calls))
    for call in sample(calls, args.count, random.Random(args.seed)):
        print(json.dumps(call, ensure_ascii=False))


if __name__ == "__main__":
    main()
