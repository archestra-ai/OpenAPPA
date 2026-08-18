"""Aggregation: per-agent utility and attack-success rates, plus printed tables.

Utility is averaged over episodes of scenarios that declare utility checks;
ASR over episodes of scenarios that declare security checks. Episodes that
errored still contribute (their end state is what it is); the error count is
reported alongside so a low utility from crashes is visible as such.

The per-agent rates compress away the thing that usually matters — *which*
scenarios moved. A scenario every arm passes (or every arm fails) hands each
arm the same points and hides the spread, so the per-scenario table marks
those rows rather than leaving the reader to diff the columns. The mark is
computed from the run, never from a hardcoded list: which scenarios separate
the arms is exactly what a remediation is supposed to change.

`events` counts policy interactions scraped from the demos' logs, not blocks:
APPA's one feedback channel carries refusals, acknowledgements and join
notices alike. It explains the numbers; it never affects a score.
"""

from __future__ import annotations

import json
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

from .runner import EpisodeResult, episode_record


@dataclass(frozen=True)
class AgentSummary:
    agent: str
    episodes: int
    errors: int
    utility_passed: int
    utility_total: int
    attacks_succeeded: int
    attacks_total: int
    mean_duration_s: float
    policy_events: int
    remedy_calls: int


def summarize(results: list[EpisodeResult]) -> list[AgentSummary]:
    by_agent: dict[str, list[EpisodeResult]] = defaultdict(list)
    for result in results:
        by_agent[result.agent].append(result)
    summaries = []
    for agent, episodes in sorted(by_agent.items()):
        utility = [r.utility for r in episodes if r.utility is not None]
        security = [r.security for r in episodes if r.security is not None]
        summaries.append(
            AgentSummary(
                agent=agent,
                episodes=len(episodes),
                errors=sum(1 for r in episodes if r.error),
                utility_passed=sum(utility),
                utility_total=len(utility),
                attacks_succeeded=sum(security),
                attacks_total=len(security),
                mean_duration_s=round(sum(r.duration_s for r in episodes) / len(episodes), 1),
                policy_events=sum(r.policy_events for r in episodes),
                remedy_calls=sum(r.remedy_calls for r in episodes),
            )
        )
    return summaries


def _rate(passed: int, total: int) -> str:
    if total == 0:
        return "  —  "
    return f"{passed}/{total} ({100 * passed / total:3.0f}%)"


def print_scenario_table(results: list[EpisodeResult]) -> None:
    """Per-scenario utility outcomes, one column per agent.

    ``=`` marks a scenario whose arms all landed the same way: it separated
    nothing in this run and contributed no signal to the rates above.
    """
    agents = sorted({r.agent for r in results})
    scenarios = sorted({r.scenario for r in results})
    if not agents or not scenarios:
        return
    # rep-collapsed: a cell passes when every rep of it passed.
    cells: dict[tuple[str, str], bool | None] = {}
    for scenario in scenarios:
        for agent in agents:
            outcomes = [r.utility for r in results if r.scenario == scenario and r.agent == agent]
            present = [o for o in outcomes if o is not None]
            cells[(scenario, agent)] = all(present) if present else None

    width = max(len(s) for s in scenarios) + 2
    columns = max(max(len(a) for a in agents), 5) + 2
    print()
    print("utility by scenario (T pass / F fail / – no utility check; = arms all equal)")
    print(f"{'scenario':<{width}}" + "".join(f"{a:>{columns}}" for a in agents) + "   ")
    for scenario in scenarios:
        row = [cells[(scenario, agent)] for agent in agents]
        present = [value for value in row if value is not None]
        flat = "=" if len(set(present)) <= 1 else " "
        marks = {True: "T", False: "F", None: "–"}
        print(
            f"{scenario:<{width}}"
            + "".join(f"{marks[value]:>{columns}}" for value in row)
            + f"   {flat}"
        )


def print_table(summaries: list[AgentSummary]) -> None:
    header = f"{'agent':<12} {'utility':>14} {'ASR':>14} {'errors':>7} {'mean s':>7} {'events':>8} {'remedies':>9}"
    print(header)
    print("-" * len(header))
    for s in summaries:
        print(
            f"{s.agent:<12} {_rate(s.utility_passed, s.utility_total):>14} "
            f"{_rate(s.attacks_succeeded, s.attacks_total):>14} {s.errors:>7} "
            f"{s.mean_duration_s:>7} {s.policy_events:>8} {s.remedy_calls:>9}"
        )


def write_summary(run_dir: Path, summaries: list[AgentSummary], results: list[EpisodeResult]) -> None:
    (run_dir / "summary.json").write_text(
        json.dumps(
            {
                "agents": [s.__dict__ for s in summaries],
                "episodes": [episode_record(r) for r in results],
            },
            indent=2,
        )
        + "\n"
    )
