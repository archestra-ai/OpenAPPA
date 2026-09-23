"""Connect shared live concurrency control to Inspect's public control API."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

from appa_bench_concurrency import AdaptiveConcurrency
from inspect_ai.hooks import Hooks, SampleEnd, SampleStart, TaskStart, hooks

EVENTS_ENV = "APPA_BENCH_CONCURRENCY_EVENTS"
_active_admission = False


def _append_event(payload: dict[str, object]) -> None:
    path = os.environ.get(EVENTS_ENV)
    if path is None:
        return
    descriptor = os.open(path, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
    try:
        os.write(descriptor, (json.dumps(payload, sort_keys=True) + "\n").encode())
    finally:
        os.close(descriptor)


@hooks(name="appa-adaptive-concurrency", description="Tune benchmark sample admission from live memory pressure")
class AdaptiveConcurrencyHooks(Hooks):
    async def on_task_start(self, data: TaskStart) -> None:
        _append_event({"kind": "task", "task_id": data.spec.task_id})

    async def on_sample_start(self, data: SampleStart) -> None:
        _append_event({"kind": "started"})

    async def on_sample_end(self, data: SampleEnd) -> None:
        error = data.sample.error
        text = "" if error is None else str(error).lower()
        _append_event(
            {
                "kind": "completed",
                "clean": error is None,
                "throttled": "429" in text or "rate limit" in text,
            }
        )


class InspectAdmission:
    def __init__(
        self,
        maximum: int,
        output_dir: Path,
        *,
        controller: AdaptiveConcurrency | None = None,
    ) -> None:
        self.output_dir = output_dir
        self.controller = controller or AdaptiveConcurrency(maximum, history_path=output_dir / "concurrency.jsonl")
        self._events_path = output_dir / "inspect-concurrency-events.jsonl"
        self._events_offset = 0
        self._task_id: str | None = None
        self._previous_events_env: str | None = None
        self.control_failures: list[str] = []
        self._applied = 1
        self._apply_lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._monitor, name="inspect-memory-admission", daemon=True)

    def __enter__(self):
        global _active_admission
        if _active_admission:
            raise RuntimeError("an adaptive Inspect evaluation is already active")
        _active_admission = True
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self._events_path.write_text("", encoding="utf-8")
        self._previous_events_env = os.environ.get(EVENTS_ENV)
        os.environ[EVENTS_ENV] = str(self._events_path)
        self._thread.start()
        return self

    def completed(self, *, clean: bool, throttled: bool) -> None:
        self.controller.completed(clean=clean, throttled=throttled)
        self._apply_if_changed()

    def _monitor(self) -> None:
        next_memory_check = 0.0
        while not self._stop.wait(0.05):
            self._read_events()
            if time.monotonic() >= next_memory_check:
                self.controller.observe()
                self._apply_if_changed()
                next_memory_check = time.monotonic() + 1.0

    def _read_events(self, *, apply: bool = True) -> None:
        try:
            with self._events_path.open(encoding="utf-8") as handle:
                handle.seek(self._events_offset)
                lines = handle.readlines()
                self._events_offset = handle.tell()
        except FileNotFoundError:
            return
        for line in lines:
            event = json.loads(line)
            if event["kind"] == "task":
                self._task_id = str(event["task_id"])
            elif event["kind"] == "started":
                self.controller.started()
            elif not apply:
                self.controller.completed(
                    clean=bool(event["clean"]),
                    throttled=bool(event["throttled"]),
                )
            else:
                self.completed(
                    clean=bool(event["clean"]),
                    throttled=bool(event["throttled"]),
                )

    def _apply_if_changed(self) -> None:
        with self._apply_lock:
            target = self.controller.limit
            if target == self._applied or self._task_id is None:
                return
            command = [
                str(Path(sys.executable).with_name("inspect")),
                "ctl",
                "config",
                self._task_id,
                "--max-samples",
                str(target),
                "--author",
                "appa-bench-concurrency",
                "--reason",
                self.controller.transitions[-1].reason,
                "--json",
            ]
            result = subprocess.run(command, capture_output=True, text=True, check=False)
            try:
                response = json.loads(result.stdout)
            except json.JSONDecodeError:
                response = None
            if result.returncode == 0 and isinstance(response, dict) and response.get("applied") is True:
                self._applied = target
            else:
                self.control_failures.append(
                    f"max_samples={target}: exit={result.returncode} response={result.stdout.strip()} "
                    f"error={result.stderr.strip()}"
                )

    def __exit__(self, exc_type, exc_value, traceback) -> None:
        global _active_admission
        self._stop.set()
        self._thread.join()
        self._read_events(apply=False)
        if self._previous_events_env is None:
            os.environ.pop(EVENTS_ENV, None)
        else:
            os.environ[EVENTS_ENV] = self._previous_events_env
        _active_admission = False
        summary = self.controller.summary()
        summary["control_failures"] = self.control_failures
        (self.output_dir / "concurrency-summary.json").write_text(
            json.dumps(summary, indent=2) + "\n",
            encoding="utf-8",
        )
