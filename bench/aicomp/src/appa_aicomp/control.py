"""What each triage sub-agent handed back to the parent, per model and arm.

    uv run python -m appa_aicomp.control runs/r*/triage-*/
"""

import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

from appa_aicomp.quarantine import RETURN_SCHEMA, TRIAGE_TOOL

FIELDS = set(RETURN_SCHEMA["properties"])
SHAPES = ("the four fields", "other JSON", "not JSON", "no answer")


def shape(event: dict) -> str:
    if not event["ok"]:
        return "no answer"
    try:
        value = json.loads(str(event["output"]))
    except json.JSONDecodeError:
        return "not JSON"
    match value:
        case dict() if set(value) == FIELDS:
            return "the four fields"
        case _:
            return "other JSON"


def main() -> None:
    shapes: dict[tuple[str, str], Counter] = defaultdict(Counter)
    for d in map(Path, sys.argv[1:]):
        for line in (d / "rows.jsonl").read_text().splitlines():
            row = json.loads(line)
            for event in row["trace"]["tool_events"]:
                if event["name"] == TRIAGE_TOOL:
                    shapes[(row["model"], row["arm"])][shape(event)] += 1
    print("| model | arm | " + " | ".join(SHAPES) + " |")
    print("|---" * (len(SHAPES) + 2) + "|")
    for (model, arm), counts in sorted(shapes.items()):
        print(f"| {model} | {arm} | " + " | ".join(str(counts[s]) for s in SHAPES) + " |")


if __name__ == "__main__":
    main()
