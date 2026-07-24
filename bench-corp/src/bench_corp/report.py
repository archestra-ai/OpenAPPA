"""Aggregation: per-SUT utility and attack-success rates, plus a printed table.

Utility is averaged over episodes of scenarios that declare utility checks;
ASR over episodes of scenarios that declare security checks. Episodes that
errored still contribute (their end state is what it is); the error count is
reported alongside so a low utility from crashes is visible as such.
"""

from __future__ import annotations

import json
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

from .runner import EpisodeResult


@dataclass(frozen=True)
class SutSummary:
    sut: str
    episodes: int
    errors: int
    utility_passed: int
    utility_total: int
    attacks_succeeded: int
    attacks_total: int
    mean_duration_s: float
    blocked_lines: int
    remedy_calls: int


def summarize(results: list[EpisodeResult]) -> list[SutSummary]:
    by_sut: dict[str, list[EpisodeResult]] = defaultdict(list)
    for result in results:
        by_sut[result.sut].append(result)
    summaries = []
    for sut, episodes in sorted(by_sut.items()):
        utility = [r.utility for r in episodes if r.utility is not None]
        security = [r.security for r in episodes if r.security is not None]
        summaries.append(
            SutSummary(
                sut=sut,
                episodes=len(episodes),
                errors=sum(1 for r in episodes if r.error),
                utility_passed=sum(utility),
                utility_total=len(utility),
                attacks_succeeded=sum(security),
                attacks_total=len(security),
                mean_duration_s=round(sum(r.duration_s for r in episodes) / len(episodes), 1),
                blocked_lines=sum(r.blocked_lines for r in episodes),
                remedy_calls=sum(r.remedy_calls for r in episodes),
            )
        )
    return summaries


def _rate(passed: int, total: int) -> str:
    if total == 0:
        return "  —  "
    return f"{passed}/{total} ({100 * passed / total:3.0f}%)"


def print_table(summaries: list[SutSummary]) -> None:
    header = f"{'SUT':<12} {'utility':>14} {'ASR':>14} {'errors':>7} {'mean s':>7} {'blocked':>8} {'remedies':>9}"
    print(header)
    print("-" * len(header))
    for s in summaries:
        print(
            f"{s.sut:<12} {_rate(s.utility_passed, s.utility_total):>14} "
            f"{_rate(s.attacks_succeeded, s.attacks_total):>14} {s.errors:>7} "
            f"{s.mean_duration_s:>7} {s.blocked_lines:>8} {s.remedy_calls:>9}"
        )


def write_summary(run_dir: Path, summaries: list[SutSummary], results: list[EpisodeResult]) -> None:
    (run_dir / "summary.json").write_text(
        json.dumps(
            {
                "suts": [s.__dict__ for s in summaries],
                "episodes": [
                    {k: v for k, v in r.__dict__.items() if k != "checks"} for r in results
                ],
            },
            indent=2,
        )
        + "\n"
    )
