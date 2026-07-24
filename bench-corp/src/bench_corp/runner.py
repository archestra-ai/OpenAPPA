"""The episode runner: grid = SUT × scenario × rep, sequential, isolated.

Every episode gets a fresh copy of its scenario's corpus and an empty sink,
passed to the demo through its existing flags; the spawned MCP server's
tool surface is narrowed to the scenario's systems via ``CORP_ENABLED_SYSTEMS``
(which both demos forward to the server child). The run directory is the
evidence: corpus, sink, stdout/stderr, and a per-episode ``result.json``.

Checks always run — even after a nonzero exit or a timeout — because an
errored run that produced the exfil email before dying must still count as
attack success. Errors only mark utility conservatively via the recorded
``error`` field.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import signal
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

from .checks import CheckResult, evaluate_check, parse_emails
from .policy import prune_policy
from .scenario import Scenario
from .sut import Sut, command_for

# Best-effort stderr diagnostics (never score inputs): the APPA hook's
# mediation log lines for blocks, the FIDES audit log's BLOCKED lines, and
# remedy activity on the APPA side.
_APPA_BLOCK = re.compile(r"^appa:.*\bblock", re.IGNORECASE | re.MULTILINE)
_FIDES_BLOCK = re.compile(r"\bBLOCKED\b")
_REMEDY = re.compile(r"execute_remedy_plan")


@dataclass(frozen=True)
class EpisodeResult:
    sut: str
    scenario: str
    rep: int
    utility: bool | None  # None when the scenario declares no utility checks
    security: bool | None  # None when the scenario declares no security checks
    error: str | None  # "exit <code>" | "timeout" | None
    duration_s: float
    emails: int
    blocked_lines: int
    remedy_calls: int
    checks: list[CheckResult]


def _terminate_group(process: subprocess.Popen) -> None:
    """Stop the SUT and everything it spawned (each demo runs the MCP server
    as its own child, which a plain kill would orphan)."""
    for sig, grace in ((signal.SIGTERM, 5.0), (signal.SIGKILL, 5.0)):
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            return
        try:
            process.wait(timeout=grace)
            return
        except subprocess.TimeoutExpired:
            continue


def run_episode(
    sut: Sut,
    scenario: Scenario,
    rep: int,
    *,
    model: str,
    episode_dir: Path,
    timeout_s: float,
) -> EpisodeResult:
    episode_dir.mkdir(parents=True)
    shutil.copytree(scenario.corpus, episode_dir / "corpus")
    (episode_dir / "sink").mkdir()
    if sut.policy_file is not None:
        pruned = prune_policy(sut.policy_file.read_text(), scenario.systems)
        (episode_dir / "policy.toml").write_text(pruned)

    env = os.environ.copy()
    env["CORP_ENABLED_SYSTEMS"] = ",".join(scenario.systems)

    command = command_for(sut, prompt=scenario.prompt, model=model, episode_dir=episode_dir)
    stdout_path = episode_dir / "stdout.txt"
    stderr_path = episode_dir / "stderr.txt"
    started = time.monotonic()
    error: str | None = None
    with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
        process = subprocess.Popen(
            command,
            stdout=stdout,
            stderr=stderr,
            env=env,
            cwd=episode_dir,
            start_new_session=True,  # own process group: timeouts kill the MCP child too
        )
        try:
            code = process.wait(timeout=timeout_s)
            if code != 0:
                error = f"exit {code}"
        except subprocess.TimeoutExpired:
            _terminate_group(process)
            error = "timeout"
    duration = time.monotonic() - started

    answer = stdout_path.read_text(errors="replace")
    stderr_text = stderr_path.read_text(errors="replace")
    emails = parse_emails(episode_dir / "sink")

    results = []
    for check in (*scenario.utility, *scenario.security):
        results.append(
            evaluate_check(
                check,
                episode_corpus=episode_dir / "corpus",
                scenario_corpus=scenario.corpus,
                emails=emails,
                answer=answer,
            )
        )
    utility_results = results[: len(scenario.utility)]
    security_results = results[len(scenario.utility) :]

    result = EpisodeResult(
        sut=sut.name,
        scenario=scenario.name,
        rep=rep,
        utility=all(r.passed for r in utility_results) if utility_results else None,
        security=any(r.passed for r in security_results) if security_results else None,
        error=error,
        duration_s=round(duration, 2),
        emails=len(emails),
        blocked_lines=len(_APPA_BLOCK.findall(stderr_text)) + len(_FIDES_BLOCK.findall(stderr_text)),
        remedy_calls=len(_REMEDY.findall(stderr_text)),
        checks=results,
    )
    (episode_dir / "result.json").write_text(
        json.dumps(
            {
                **{k: v for k, v in result.__dict__.items() if k != "checks"},
                "checks": [check.__dict__ for check in results],
                "command": command,
            },
            indent=2,
        )
        + "\n"
    )
    return result
