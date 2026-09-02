"""Execute benchmark arms, preserve per-command evidence, and render scores."""

from __future__ import annotations

import asyncio
import hashlib
import json
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import AsyncExitStack
from dataclasses import asdict
from pathlib import Path
from typing import Any

from mcp import ClientSession
from mcp.client.streamable_http import streamable_http_client

from . import BENCHMARK_VERSION, DATASET_ID, DATASET_REVISION
from .dataset import CommandRow
from .policy import ModelProfile, annotation_policy, authority_policy
from .runtime import BARE_PROMPT, BareOpenAiClient, RuntimeProcess
from .scoring import Outcome, summarize

ARMS = ("annotator", "authority", "bare")


def _git_state(repo: Path) -> dict[str, Any]:
    def run(*arguments: str) -> str:
        return subprocess.run(["git", *arguments], cwd=repo, check=False, capture_output=True, text=True).stdout.strip()

    return {"git_sha": run("rev-parse", "HEAD"), "git_dirty": bool(run("status", "--porcelain"))}


def _write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n")


class RecordSink:
    def __init__(self, path: Path, rows: dict[str, CommandRow]) -> None:
        self.path = path
        self.temporary = path.with_suffix(path.suffix + ".tmp")
        self.rows = rows
        self.lock = threading.Lock()
        self.file = self.temporary.open("w")

    def add(self, outcome: Outcome) -> None:
        value = {**asdict(self.rows[outcome.id]), **asdict(outcome)}
        with self.lock:
            self.file.write(json.dumps(value, separators=(",", ":"), ensure_ascii=False) + "\n")
            self.file.flush()

    def close(self, *, complete: bool) -> None:
        self.file.close()
        if complete:
            self.temporary.replace(self.path)


def _progress(arm: str, index: int, total: int, outcome: Outcome) -> None:
    prediction = outcome.prediction or "no-answer"
    print(f"[{arm} {index}/{total}] {outcome.id} -> {prediction} ({outcome.latency_ms:.1f} ms)", file=sys.stderr)


def _parallel(
    arm: str,
    rows: list[CommandRow],
    classify: Any,
    *,
    jobs: int,
    sink: RecordSink,
) -> list[Outcome]:
    def one(index_and_row: tuple[int, CommandRow]) -> Outcome:
        index, row = index_and_row
        try:
            outcome = classify(row)
        except Exception as error:  # preserve a complete benchmark prediction file on per-item failures
            outcome = Outcome(row.id, row.source, row.label, None, "client_error", 0.0, str(error))
        sink.add(outcome)
        _progress(arm, index, len(rows), outcome)
        return outcome

    with ThreadPoolExecutor(max_workers=jobs) as executor:
        return list(executor.map(one, enumerate(rows, start=1)))


async def _authority_results(
    runtime: RuntimeProcess,
    rows: list[CommandRow],
    sink: RecordSink,
) -> list[Outcome]:
    outcomes: list[Outcome] = []
    async with AsyncExitStack() as stack:
        read, write, _ = await stack.enter_async_context(
            streamable_http_client(f"{runtime.base_url}/mcp", terminate_on_close=False)
        )
        session = await stack.enter_async_context(ClientSession(read, write))
        await session.initialize()
        for index, row in enumerate(rows, start=1):
            try:
                offer, failure = runtime.prepare_authority(row)
                if failure is not None:
                    outcome = failure
                else:
                    assert offer is not None
                    started = time.perf_counter()
                    result = await session.call_tool("execute_remedy_plan", {"offer_id": offer.id})
                    latency = (time.perf_counter() - started) * 1_000
                    text = "\n".join(
                        content.text for content in result.content if isinstance(getattr(content, "text", None), str)
                    )
                    if getattr(result, "isError", False) or getattr(result, "is_error", False):
                        outcome = Outcome(row.id, row.source, row.label, None, "remedy_refusal", latency, text)
                    elif "gave no answer" in text:
                        outcome = Outcome(row.id, row.source, row.label, None, "no_answer", latency, text)
                    elif "Authorized." in text:
                        outcome = Outcome(row.id, row.source, row.label, "not_risky", "approved", latency, text)
                    else:
                        outcome = Outcome(row.id, row.source, row.label, "risky", "denied", latency, text)
            except Exception as error:
                outcome = Outcome(row.id, row.source, row.label, None, "client_error", 0.0, str(error))
            sink.add(outcome)
            outcomes.append(outcome)
            _progress("authority", index, len(rows), outcome)
    return outcomes


def _write_arm_outputs(directory: Path, outcomes: list[Outcome]) -> dict[str, Any]:
    summary = summarize(outcomes)
    _write_json(directory / "summary.json", summary)
    for name, fail_closed in [("predictions.jsonl", False), ("predictions-fail-closed.jsonl", True)]:
        with (directory / name).open("w") as predictions:
            for outcome in outcomes:
                prediction = outcome.prediction or ("risky" if fail_closed else "not_risky")
                predictions.write(
                    json.dumps({"id": outcome.id, "prediction": prediction}, separators=(",", ":")) + "\n"
                )
    return summary


def run_evaluation(
    *,
    repo: Path,
    rows: list[CommandRow],
    arms: list[str],
    profile: ModelProfile,
    appa_bin: Path,
    run_dir: Path,
    jobs: int,
) -> dict[str, Any]:
    run_dir.mkdir(parents=True, exist_ok=False)
    manifest = {
        "benchmark": "ShellRisk-Bench",
        "benchmark_version": BENCHMARK_VERSION,
        "dataset_id": DATASET_ID,
        "dataset_revision": DATASET_REVISION,
        "selection": {"n": len(rows), "ids": [row.id for row in rows]},
        "arms": arms,
        "bare_prompt_sha256": hashlib.sha256(BARE_PROMPT.encode()).hexdigest(),
        "model": {
            "provider": profile.provider,
            "model": profile.model,
            "url": profile.url,
            "token_env": profile.token_env,
            "timeout_ms": profile.timeout_ms,
            "max_concurrent": profile.max_concurrent,
        },
        "jobs": jobs,
        **_git_state(repo),
    }
    _write_json(run_dir / "manifest.json", manifest)
    summaries: dict[str, Any] = {}
    indexed = {row.id: row for row in rows}

    for arm in arms:
        directory = run_dir / arm
        sink: RecordSink | None = None
        complete = False
        try:
            if arm == "annotator":
                runtime = RuntimeProcess(
                    appa_bin=appa_bin,
                    policy=annotation_policy(profile),
                    profile=profile,
                    directory=directory,
                )
                with runtime:
                    sink = RecordSink(directory / "records.jsonl", indexed)
                    outcomes = _parallel(arm, rows, runtime.evaluate_annotation, jobs=jobs, sink=sink)
            elif arm == "authority":
                runtime = RuntimeProcess(
                    appa_bin=appa_bin,
                    policy=authority_policy(profile),
                    profile=profile,
                    directory=directory,
                )
                with runtime:
                    sink = RecordSink(directory / "records.jsonl", indexed)
                    outcomes = asyncio.run(_authority_results(runtime, rows, sink))
            elif arm == "bare":
                directory.mkdir(parents=True, exist_ok=False)
                sink = RecordSink(directory / "records.jsonl", indexed)
                outcomes = _parallel(arm, rows, BareOpenAiClient(profile).classify, jobs=jobs, sink=sink)
            else:
                raise ValueError(f"unknown ShellRisk arm {arm!r}")
            complete = True
            summaries[arm] = _write_arm_outputs(directory, outcomes)
        finally:
            if sink is not None:
                sink.close(complete=complete)
    _write_json(run_dir / "summary.json", summaries)
    return summaries
