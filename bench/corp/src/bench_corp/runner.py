"""The episode runner: grid = agent × scenario × rep, independently isolated.

Every episode gets a fresh copy of its scenario's data and an empty sink,
passed to the demo through its existing flags; the spawned MCP server's
tool surface is narrowed to the scenario's systems via ``CORP_ENABLED_SYSTEMS``
(which both demos forward to the server child). The run directory is the
evidence: data, sink, stdout/stderr, and a per-episode ``result.json``.

Checks always run — even after a nonzero exit or a timeout — because an
errored run that produced the exfil email before dying must still count as
attack success. A controlled APPA budget finalization is recorded separately
from process, provider, runtime, and runner errors; all of them retain their
observed end-state scores.
"""

from __future__ import annotations

import contextlib
import json
import os
import re
import shutil
import signal
import subprocess
import threading
import time
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Iterator

from . import AGENT_PROMPT_PROFILES
from .agents import Agent, PolicyTarget, command_for
from .checks import CheckResult, evaluate_check, parse_emails
from .policy import apply_tool_requires, bind_external_urls, prune_policy
from .scenario import AnnotatorAnswer, AuthorityAnswer, SanitizerAnswer, Scenario, canonical_args

# Best-effort stderr diagnostics (never score inputs): the APPA hook's
# mediation log lines, the FIDES audit log's BLOCKED lines, and executed
# remedies on the APPA side. Anchored to the exact log wording (pinned by a
# test against literal copies of the real lines): a looser remedy pattern
# would also count the demo's startup banner and every block-feedback line,
# which both mention execute_remedy_plan.
#
# The APPA count is *policy events*, not blocks. `Fact::BlockFeedback` carries
# no semantic kind — it is the one channel for refusals, acknowledgements
# (a void return's "no result returned to the parent") and join notifications
# alike — so the demo prints them identically and no regex can separate them.
# Counting them together is honest; calling the total "blocked" was not.
_APPA_POLICY_EVENT = re.compile(r"^appa:.*\bblock", re.IGNORECASE | re.MULTILINE)
_FIDES_BLOCK = re.compile(r"\bBLOCKED\b")
_REMEDY = re.compile(r"^appa: remedy authorized\b", re.MULTILINE)
_PROVIDER_ATTEMPTS = re.compile(r"^appa:.*inference completed after (\d+) provider attempts$", re.MULTILINE)
_TERMINAL_STATUSES = {
    "completed",
    "budget_finalized",
    "provider_failed",
    "runtime_refused",
    "cancelled",
    "budget_exhausted",
}
_FAILED_TERMINAL_STATUSES = {"provider_failed", "runtime_refused", "cancelled", "budget_exhausted"}


def _count(pattern: re.Pattern[str], text: str) -> int:
    return sum(1 for _ in pattern.finditer(text))


def _provider_retries(text: str) -> int:
    return sum(max(0, int(match.group(1)) - 1) for match in _PROVIDER_ATTEMPTS.finditer(text))


@dataclass(frozen=True)
class EpisodeResult:
    agent: str
    scenario: str
    rep: int
    agent_prompt_profile: str
    utility: bool | None  # None when the scenario declares no utility checks
    security: bool | None  # None when the scenario declares no security checks
    error: str | None  # "exit <code>" | "timeout" | None
    terminal_status: str | None  # APPA's typed status; absent for agents without the contract
    duration_s: float
    emails: int
    answer_present: bool  # the agent printed a final answer (FIDES leaves it empty when blocked)
    policy_events: int
    remedy_calls: int
    provider_retries: int
    checks: list[CheckResult]


def episode_record(result: EpisodeResult) -> dict:
    """The JSON-ready scalar fields of a result (checks serialize separately)."""
    return {k: v for k, v in result.__dict__.items() if k != "checks"}


def _terminate_group(process: subprocess.Popen) -> None:
    """Stop the agent and everything it spawned (each demo runs the MCP server
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


def _consult_artifact(request: dict, kind: str, name: str) -> dict:
    """The artifact of a consult envelope addressed to this external.

    Every consult arrives wrapped: the envelope names the kind and the
    component, and the artifact is the value that component judges. An
    envelope addressed elsewhere yields nothing to match on, so the fixture
    answers 404 — and no answer grants nothing.
    """
    if request.get("version") != 1 or request.get("kind") != kind or request.get("name") != name:
        return {}
    artifact = request.get("artifact")
    return artifact if isinstance(artifact, dict) else {}


@contextlib.contextmanager
def _serve_external_fixtures(
    annotator_answers: tuple[AnnotatorAnswer, ...],
    authority_answers: tuple[AuthorityAnswer, ...],
    sanitizer_answers: tuple[SanitizerAnswer, ...],
    request_log: Path,
) -> Iterator[str | None]:
    """Serve scenario-owned externals on an isolated loopback port."""
    if not (annotator_answers or authority_answers or sanitizer_answers):
        yield None
        return

    # Each answer carries the verbatim annotation its scenario declares for that consult,
    # validated against the policy at scenario load — the fixture keeps no second copy of
    # the contract.
    annotation_by_request = {answer.request_key: answer.annotation for answer in annotator_answers}
    authority_by_request = {
        (answer.authority, answer.tool): answer.ruling for answer in authority_answers
    }
    sanitizer_by_name = {answer.sanitizer: answer for answer in sanitizer_answers}
    request_log.touch()
    log_lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802 — BaseHTTPRequestHandler's protocol method
            response: dict | None = None
            kind = "unknown"
            name = ""
            try:
                length = int(self.headers.get("Content-Length", ""))
                if length < 0 or length > 64 * 1024:
                    raise ValueError
                request = json.loads(self.rfile.read(length))
            except (KeyError, TypeError, ValueError, json.JSONDecodeError):
                request = None

            if isinstance(request, dict) and self.path == "/annotator":
                kind = "annotation"
                name = str(request.get("name", ""))
                # The consult carries no tool name: the annotator and the exact `args` it was
                # sent are the whole key.
                artifact = _consult_artifact(request, "annotation", name)
                annotation = annotation_by_request.get((name, canonical_args(artifact.get("args")))) if artifact else None
                if annotation is not None:
                    response = {"version": 1, "answer": json.loads(annotation)}
            elif isinstance(request, dict) and self.path.startswith("/authority/"):
                kind = "authority"
                name = self.path.removeprefix("/authority/")
                artifact = _consult_artifact(request, kind, name)
                ruling = authority_by_request.get((name, artifact.get("tool"))) if artifact else None
                if ruling is not None:
                    response = {"version": 1, "answer": {"ruling": ruling}}
            elif isinstance(request, dict) and self.path.startswith("/sanitizer/"):
                kind = "sanitizer"
                name = self.path.removeprefix("/sanitizer/")
                artifact = _consult_artifact(request, kind, name)
                answer = sanitizer_by_name.get(name)
                body = artifact.get("body")
                if answer is not None and isinstance(body, str):
                    derived = "".join(
                        line
                        for line in body.splitlines(keepends=True)
                        if not any(needle in line for needle in answer.drop_lines_containing)
                    )
                    response = {"version": 1, "answer": {"body": derived}}

            status = 200 if response is not None else 404
            record = {"kind": kind, "name": name, "request": request, "status": status}
            if response is not None:
                record["response"] = response
            with log_lock:
                with request_log.open("a") as log:
                    log.write(json.dumps(record, sort_keys=True) + "\n")

            response_body = json.dumps(response).encode() if response is not None else b"{}"
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(response_body)))
            self.end_headers()
            self.wfile.write(response_body)

        def log_message(self, format: str, *args: object) -> None:
            return

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
    thread.start()
    try:
        host, port = server.server_address
        yield f"http://{host}:{port}"
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def _stage_policy(
    agent: Agent,
    scenario: Scenario,
    episode_dir: Path,
    external_fixture_origin: str | None = None,
) -> Path | None:
    """The policy this episode runs under, written into the episode directory.

    An APPA arm gets the pruned TOML — the scenario's own profile when it ships
    one, else the shared benchmark policy — with any `requires` the scenario's
    deployment declares for that arm applied on top. FIDES gets the profile's
    JSON, and nothing when the scenario ships no profile.
    """
    match agent.policy_target:
        case PolicyTarget.APPA_GUARDED:
            source = scenario.policy_profile.appa if scenario.policy_profile is not None else agent.policy_file
        case PolicyTarget.APPA_OPEN:
            source = agent.policy_file
        case PolicyTarget.FIDES:
            if scenario.policy_profile is None:
                return None
            destination = episode_dir / "fides.json"
            shutil.copyfile(scenario.policy_profile.fides, destination)
            return destination
        case PolicyTarget.NONE:
            return None

    if source is None or agent.policy_file is None:
        raise ValueError(f"{agent.name}: APPA agents require a source policy")
    pruned = prune_policy(source.read_text(), scenario.systems)
    # Keyed by the arm's shared policy stem even when a profile supplied the
    # source: a scenario declares the gate for `appa`, not for whichever file
    # the episode happened to read it from.
    pruned = apply_tool_requires(pruned, scenario.policy_requires.get(agent.policy_file.stem, {}))
    if external_fixture_origin is not None and agent.policy_target == PolicyTarget.APPA_GUARDED:
        pruned = bind_external_urls(pruned, external_fixture_origin)
    destination = episode_dir / "policy.toml"
    destination.write_text(pruned)
    return destination


def run_episode(
    agent: Agent,
    scenario: Scenario,
    rep: int,
    *,
    model: str,
    episode_dir: Path,
    timeout_s: float,
    agent_prompt_profile: str = "standard",
) -> EpisodeResult:
    if agent_prompt_profile not in AGENT_PROMPT_PROFILES:
        raise ValueError(f"unknown agent prompt profile: {agent_prompt_profile}")
    episode_dir = episode_dir.resolve()
    episode_dir.mkdir(parents=True)
    shutil.copytree(scenario.data, episode_dir / "data")
    (episode_dir / "sink").mkdir()
    stdout_path = episode_dir / "stdout.txt"
    stderr_path = episode_dir / "stderr.txt"
    external_request_log = episode_dir / "external-requests.jsonl"
    with _serve_external_fixtures(
        scenario.annotator_answers,
        scenario.authority_answers,
        scenario.sanitizer_answers,
        external_request_log,
    ) as external_fixture_origin:
        policy_path = _stage_policy(agent, scenario, episode_dir, external_fixture_origin)

        env = os.environ.copy()
        env["CORP_ENABLED_SYSTEMS"] = ",".join(scenario.systems)
        env["APPA_AGENT_PROMPT_ADDENDUM"] = AGENT_PROMPT_PROFILES[agent_prompt_profile]

        command = command_for(
            agent,
            prompt=scenario.prompt,
            model=model,
            episode_dir=episode_dir,
            policy_path=policy_path,
        )
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
    terminal_status = None
    status_path = episode_dir / "agent-status.json"
    if status_path.is_file():
        try:
            status_record = json.loads(status_path.read_text())
            if status_record.get("version") != 1 or status_record.get("status") not in _TERMINAL_STATUSES:
                raise ValueError("unsupported terminal status")
            terminal_status = status_record["status"]
        except (OSError, ValueError, json.JSONDecodeError):
            terminal_status = "invalid_status"
            error = error or "invalid agent status"
    if terminal_status in _FAILED_TERMINAL_STATUSES:
        error = terminal_status
    emails = parse_emails(episode_dir / "sink")
    external_requests = (
        [json.loads(line) for line in external_request_log.read_text().splitlines()]
        if external_request_log.is_file()
        else []
    )

    def evaluate(check):
        return evaluate_check(
            check,
            episode_data=episode_dir / "data",
            scenario_data=scenario.data,
            emails=emails,
            answer=answer,
            sink_root=episode_dir / "sink",
            external_requests=external_requests,
        )

    utility_results = [evaluate(check) for check in scenario.utility]
    security_results = [evaluate(check) for check in scenario.security]
    results = [*utility_results, *security_results]

    result = EpisodeResult(
        agent=agent.name,
        scenario=scenario.name,
        rep=rep,
        agent_prompt_profile=agent_prompt_profile,
        utility=all(r.passed for r in utility_results) if utility_results else None,
        security=any(r.passed for r in security_results) if security_results else None,
        error=error,
        terminal_status=terminal_status,
        duration_s=round(duration, 2),
        emails=len(emails),
        answer_present=bool(answer.strip()),
        policy_events=_count(_APPA_POLICY_EVENT, stderr_text) + _count(_FIDES_BLOCK, stderr_text),
        remedy_calls=_count(_REMEDY, stderr_text),
        provider_retries=_provider_retries(stderr_text),
        checks=results,
    )
    (episode_dir / "result.json").write_text(
        json.dumps(
            {
                **episode_record(result),
                "checks": [check.__dict__ for check in results],
                "command": command,
            },
            indent=2,
        )
        + "\n"
    )
    return result
