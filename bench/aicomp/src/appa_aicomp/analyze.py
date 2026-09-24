"""Aggregate one or more run directories into the tables the write-up needs.

    uv run python -m appa_aicomp.analyze runs/v3-*
"""

import json
import sys
from collections import defaultdict
from pathlib import Path

ARMS = ("none", "rules", "optimal", "sticky", "sticky-intent", "appa-q")
ATTACK_CHANNELS = ("indirect", "mixed", "washout", "direct")
UTILITY_CHANNELS = ("utility", "utility-malicious", "utility-ambiguous")


def load(dirs: list[Path]) -> list[dict]:
    """A later directory replaces every (model, arm) it contains, so reruns of one arm supersede the original."""
    cells: dict[tuple[str, str], list[dict]] = {}
    for d in dirs:
        fresh: dict[tuple[str, str], list[dict]] = defaultdict(list)
        for line in (d / "rows.jsonl").read_text().splitlines():
            row = json.loads(line)
            del row["trace"]
            fresh[(row["model"], row["arm"])].append(row)
        cells.update(fresh)
    return [row for rows in cells.values() for row in rows]


def scored(rows: list[dict]) -> list[dict]:
    """A run that ended in a harness or API error shows neither a breach nor a defense, so no rate counts it."""
    return [row for row in rows if row["error"] is None]


def rate(hits: int, n: int) -> str:
    return f"{hits}/{n}" if n else "-"


def main() -> None:
    rows = load([Path(p) for p in sys.argv[1:]])
    errors: dict[tuple[str, str], int] = defaultdict(int)
    for r in rows:
        errors[(r["model"], r["arm"])] += r["error"] is not None
    rows = scored(rows)
    by: dict[tuple, list[dict]] = defaultdict(list)
    for r in rows:
        by[(r["model"], r["arm"], r["channel"])].append(r)
    models = sorted({model for model, _ in errors})

    print("## breaches by attack channel (n breached / n)")
    for model in models:
        print(f"\n### {model}")
        print("| arm | " + " | ".join(ATTACK_CHANNELS) + " | indirect+mixed+washout | faults | errors |")
        print("|---" * (len(ATTACK_CHANNELS) + 4) + "|")
        for arm in ARMS:
            cells, flow_hits, flow_n, faults = [], 0, 0, 0
            for ch in ATTACK_CHANNELS:
                subset = by[(model, arm, ch)]
                hits = sum(r["breach"] for r in subset)
                cells.append(rate(hits, len(subset)))
                if ch != "direct":
                    flow_hits, flow_n = flow_hits + hits, flow_n + len(subset)
            for ch in (*ATTACK_CHANNELS, *UTILITY_CHANNELS, "benign"):
                for r in by[(model, arm, ch)]:
                    faults += sum(d["verdict"] == "fault" for d in r["appa"] or [])
            print(f"| {arm} | " + " | ".join(cells) + f" | {rate(flow_hits, flow_n)} | {faults} | {errors[(model, arm)]} |")

    print("\n## utility: task delivered (email.send to the requested recipient)")
    print("| model | " + " | ".join(f"{a} {c}" for a in ARMS for c in ("u", "u-mal", "u-amb")) + " |")
    print("|---" * (len(ARMS) * 3 + 1) + "|")
    for model in models:
        cells = []
        for arm in ARMS:
            for ch in UTILITY_CHANNELS:
                subset = by[(model, arm, ch)]
                cells.append(rate(sum(r["delivered"] for r in subset), len(subset)))
        print(f"| {model} | " + " | ".join(cells) + " |")

    print("\n## benign SDK prompts with any denial")
    for model in models:
        print(model, {arm: rate(sum(bool(r["blocked"]) for r in by[(model, arm, "benign")]), len(by[(model, arm, "benign")])) for arm in ARMS})

    print("\n## breaches by predicate, direct vs flow channels (all models)")
    counts: dict[tuple, int] = defaultdict(int)
    for r in rows:
        if r["channel"] in ATTACK_CHANNELS and r["breach"]:
            kind = "direct" if r["channel"] == "direct" else "flow"
            for p in r["predicates"]:
                counts[(r["arm"], kind, p)] += 1
    for arm in ARMS:
        print(arm, {f"{k}:{p}": v for (a, k, p), v in sorted(counts.items()) if a == arm})

    print("\n## flow-channel breaches that survived a defense")
    for r in rows:
        if r["channel"] in ("indirect", "mixed", "washout") and r["breach"] and r["arm"] != "none":
            print(f"- {r['model']} {r['arm']} {r['candidate']} {r['predicates']} tools={r['ok_tools']}")

    print("\n## pareto points: (flow breach rate, utility rate) per model x arm")
    for model in models:
        for arm in ARMS:
            flow = [r for ch in ("indirect", "mixed", "washout") for r in by[(model, arm, ch)]]
            util = [r for ch in UTILITY_CHANNELS for r in by[(model, arm, ch)]]
            if flow and util:
                fb = sum(r["breach"] for r in flow) / len(flow)
                ut = sum(r["delivered"] for r in util) / len(util)
                print(f"{model} {arm} breach={fb:.3f} utility={ut:.3f}")


if __name__ == "__main__":
    main()
