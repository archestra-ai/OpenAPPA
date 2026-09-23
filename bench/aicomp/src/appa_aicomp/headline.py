"""The blog's tables: naive-user corpus attacks, and triage over every organizer email.

    uv run python -m appa_aicomp.headline --corpus runs/corpus-* --triage runs/triage-*

Within each workload a later directory owns every (model, arm) cell it contains.
"""

import argparse
import json
import re
from collections import defaultdict
from pathlib import Path

from appa_aicomp.analyze import ARMS
from appa_aicomp.naive import CORPUS, FLOW_CHANNELS, User, classify

TRIAGE_CHANNELS = ("utility", "utility-malicious", "utility-ambiguous")
URL = re.compile(r"https?://")


def load(dirs: list[Path]) -> list[dict]:
    cells: dict[tuple[str, str], list[dict]] = {}
    for d in dirs:
        fresh: dict[tuple[str, str], list[dict]] = defaultdict(list)
        for line in (d / "rows.jsonl").read_text().splitlines():
            row = json.loads(line)
            row["sent"] = [e["args"] for e in row.pop("trace")["tool_events"] if e["name"] == "email.send" and e["ok"]]
            fresh[(row["model"], row["arm"])].append(row)
        cells.update(fresh)
    return [row for rows in cells.values() for row in rows]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", nargs="+", type=Path, required=True)
    parser.add_argument("--triage", nargs="+", type=Path, required=True)
    args = parser.parse_args()

    kinds = {r["id"]: classify(r["messages"]) for r in map(json.loads, CORPUS.read_text().splitlines())}
    corpus: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for r in load(args.corpus):
        if r["channel"] in FLOW_CHANNELS and kinds.get(r["candidate"], User.NAIVE) is User.NAIVE:
            corpus[(r["model"], r["arm"])].append(r)
    triage: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for r in load(args.triage):
        if r["channel"] in TRIAGE_CHANNELS:
            triage[(r["model"], r["arm"])].append(r)

    def cell(rows: list[dict], hit: str) -> str:
        return f"{sum(bool(r[hit]) for r in rows)}/{len(rows)}" if rows else "-"

    def relayed(rows: list[dict]) -> str:
        sent = [r for r in rows if r["sent"]]
        return f"{sum(any(URL.search(json.dumps(a)) for a in r['sent']) for r in sent)}/{len(sent)}" if rows else "-"

    metrics = {
        "naive-user corpus breaches": lambda m, a: cell(corpus[(m, a)], "breach"),
        "triage breaches": lambda m, a: cell(triage[(m, a)], "breach"),
        "triage completed": lambda m, a: cell(triage[(m, a)], "delivered"),
        "sent mails carrying a URL": lambda m, a: relayed(triage[(m, a)]),
    }
    models = sorted({m for m, _ in (*corpus, *triage)})
    print("| model | metric | " + " | ".join(ARMS) + " |")
    print("|---" * (len(ARMS) + 2) + "|")
    for model in models:
        for name, metric in metrics.items():
            print(f"| {model} | {name} | " + " | ".join(metric(model, arm) for arm in ARMS) + " |")
    errors = sum(bool(r["error"]) for rows in (*corpus.values(), *triage.values()) for r in rows)
    print(f"\nrows with a harness or API error: {errors}")


if __name__ == "__main__":
    main()
