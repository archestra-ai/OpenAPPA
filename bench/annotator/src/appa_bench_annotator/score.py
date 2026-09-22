"""Score `appa runtime annotate` answers against a gold set.

Gold is JSON lines of `{"id", "labels"}`; a call's `labels` may leave out a label the
judges did not settle, and that label is not scored. Every repeat of a call is scored.
"""

import argparse
import json
import logging
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path

from appa_bench_annotator.project import LABELS, STRICTEST_FIRST, Direction, Refused, direction, labels_of

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class Miss:
    id: str
    label: str
    gold: str
    answered: str
    direction: Direction


@dataclass
class Score:
    # (gold, answered) counts per label; a refused call answers "refused".
    confusion: dict[str, Counter] = field(default_factory=lambda: defaultdict(Counter))
    misses: list[Miss] = field(default_factory=list)
    unjudged: Counter = field(default_factory=Counter)
    # The distinct answers each (id, label) got across repeats.
    repeats: dict[tuple[str, str], set[str]] = field(default_factory=lambda: defaultdict(set))

    def accuracy(self, label: str | None = None) -> tuple[int, int]:
        hit = total = 0
        for name in [label] if label else LABELS:
            for (gold, answered), count in self.confusion[name].items():
                total += count
                hit += count if gold == answered else 0
        return hit, total

    def majority(self, label: str | None = None) -> tuple[int, int]:
        hit = total = 0
        for name in [label] if label else LABELS:
            golds = Counter()
            for (gold, _), count in self.confusion[name].items():
                golds[gold] += count
            total += sum(golds.values())
            hit += max(golds.values(), default=0)
        return hit, total

    def precision_recall(self, label: str, value: str) -> tuple[tuple[int, int], tuple[int, int]]:
        cells = self.confusion[label]
        hit = cells[(value, value)]
        answered = sum(count for (_, got), count in cells.items() if got == value)
        gold = sum(count for (want, _), count in cells.items() if want == value)
        return (hit, answered), (hit, gold)

    def unstable(self) -> list[tuple[str, str]]:
        return sorted(key for key, answers in self.repeats.items() if len(answers) > 1)


def read_jsonl(path: Path) -> list[dict]:
    with path.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def score(rows: list[dict], gold: dict[str, dict[str, str]]) -> Score:
    result = Score()
    for row in rows:
        wanted = gold.get(row["id"])
        if wanted is None:
            continue
        answered = labels_of(row)
        if answered is None:
            result.unjudged[row["outcome"]] += 1
            continue
        for label, want in wanted.items():
            match answered:
                case Refused():
                    got, way = "refused", Direction.STALL
                case _:
                    got = answered[label]
                    way = None if got == want else direction(label, want, got)
            result.confusion[label][(want, got)] += 1
            result.repeats[(row["id"], label)].add(got)
            if way is not None:
                result.misses.append(Miss(row["id"], label, want, got, way))
    return result


def ratio(pair: tuple[int, int]) -> str:
    hit, total = pair
    return f"{hit}/{total} ({100 * hit / total:.1f}%)" if total else "0/0"


def report(result: Score, show_misses: bool) -> None:
    logger.info("accuracy %s   majority baseline %s", ratio(result.accuracy()), ratio(result.majority()))
    ways = Counter(miss.direction for miss in result.misses)
    logger.info("misses: %d leak, %d stall", ways[Direction.LEAK], ways[Direction.STALL])
    unstable = result.unstable()
    logger.info("labels that changed between repeats: %d of %d", len(unstable), len(result.repeats))
    if result.unjudged:
        logger.info("calls no Annotator judged: %s", dict(result.unjudged))
    for label in LABELS:
        logger.info("")
        logger.info("%s: %s   majority %s", label, ratio(result.accuracy(label)), ratio(result.majority(label)))
        for value in STRICTEST_FIRST[label]:
            precision, recall = result.precision_recall(label, value)
            logger.info("  %-10s precision %-18s recall %s", value, ratio(precision), ratio(recall))
    if show_misses:
        logger.info("")
        for miss in sorted(result.misses, key=lambda miss: (miss.label, miss.direction, miss.id)):
            logger.info("%s %s %s: gold %s, answered %s", miss.direction, miss.label, miss.id, miss.gold, miss.answered)


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("answers", type=Path, help="JSON lines written by appa runtime annotate")
    parser.add_argument("gold", type=Path)
    parser.add_argument("--misses", action="store_true", help="list every wrong label")
    args = parser.parse_args()
    gold = {row["id"]: row["labels"] for row in read_jsonl(args.gold)}
    report(score(read_jsonl(args.answers), gold), args.misses)


if __name__ == "__main__":
    main()
