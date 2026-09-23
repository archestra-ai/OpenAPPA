"""A small AIMD controller driven by current machine pressure."""

from __future__ import annotations

import json
import threading
import time
from collections.abc import Callable
from dataclasses import asdict, dataclass
from pathlib import Path


@dataclass(frozen=True)
class MemoryReading:
    used_bytes: int
    limit_bytes: int
    source: str

    @property
    def utilization(self) -> float:
        return self.used_bytes / self.limit_bytes


@dataclass(frozen=True)
class Transition:
    monotonic_s: float
    old_limit: int
    new_limit: int
    reason: str
    memory: MemoryReading | None


def _read_int(path: Path) -> int | None:
    try:
        value = path.read_text(encoding="utf-8").strip()
        return None if value == "max" else int(value)
    except (OSError, ValueError):
        return None


def read_memory() -> MemoryReading | None:
    """Read the current cgroup limit, falling back to host available memory."""
    try:
        cgroup_path = next(
            line.split("::", 1)[1].strip()
            for line in Path("/proc/self/cgroup").read_text(encoding="utf-8").splitlines()
            if "::" in line
        )
    except (OSError, StopIteration):
        cgroup_path = "/"
    current = Path("/sys/fs/cgroup") / cgroup_path.lstrip("/")
    for directory in (current, *current.parents):
        if directory == Path("/"):
            break
        used = _read_int(directory / "memory.current")
        limit = _read_int(directory / "memory.max")
        if used is not None and limit is not None and limit > 0:
            return MemoryReading(used, limit, "cgroup-v2")

    try:
        fields = {}
        for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
            key, value = line.split(":", 1)
            fields[key] = int(value.strip().split()[0]) * 1024
        total = fields["MemTotal"]
        available = fields["MemAvailable"]
        return MemoryReading(total - available, total, "host")
    except (OSError, KeyError, ValueError):
        return None


class AdaptiveConcurrency:
    """Additive increase, multiplicative decrease admission target."""

    def __init__(
        self,
        maximum: int,
        *,
        observer: Callable[[], MemoryReading | None] = read_memory,
        history_path: Path | None = None,
        high_watermark: float = 0.85,
        low_watermark: float = 0.70,
    ) -> None:
        if maximum < 1:
            raise ValueError("maximum must be at least 1")
        self.maximum = maximum
        self.limit = 1
        self.peak_active = 0
        self._active = 0
        self._observer = observer
        self._history_path = history_path
        self._high = high_watermark
        self._low = low_watermark
        self._under_pressure = False
        self._lock = threading.Lock()
        self._changed = threading.Condition(self._lock)
        self.transitions: list[Transition] = []
        self._record(1, "initial", self._observer())

    def _record(self, old: int, reason: str, memory: MemoryReading | None) -> None:
        transition = Transition(time.monotonic(), old, self.limit, reason, memory)
        self.transitions.append(transition)
        if self._history_path is not None:
            self._history_path.parent.mkdir(parents=True, exist_ok=True)
            with self._history_path.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(asdict(transition), sort_keys=True) + "\n")

    def observe(self) -> None:
        reading = self._observer()
        if reading is None:
            return
        with self._changed:
            if reading.utilization >= self._high and not self._under_pressure:
                self._under_pressure = True
                old = self.limit
                new_limit = max(1, self.limit // 2)
                self.limit = new_limit
                self._record(old, "memory-pressure", reading)
                self._changed.notify_all()
            elif reading.utilization <= self._low and self._under_pressure:
                self._under_pressure = False
                self._record(self.limit, "memory-recovered", reading)

    def started(self) -> None:
        with self._changed:
            self._active += 1
            self.peak_active = max(self.peak_active, self._active)

    def completed(self, *, clean: bool = True, throttled: bool = False) -> None:
        with self._changed:
            self._active -= 1
            if throttled:
                old = self.limit
                self.limit = max(1, self.limit // 2)
                self._record(old, "provider-throttle", self._observer())
            elif clean and not self._under_pressure and self.limit < self.maximum:
                old = self.limit
                self.limit += 1
                self._record(old, "clean-completion", self._observer())
            self._changed.notify_all()

    def wait_for_slot(self, stop: threading.Event) -> bool:
        with self._changed:
            while self._active >= self.limit and not stop.is_set():
                self._changed.wait(timeout=0.5)
            return not stop.is_set()

    def wake(self) -> None:
        with self._changed:
            self._changed.notify_all()

    def summary(self) -> dict[str, object]:
        with self._lock:
            return {
                "effective_ceiling": self.maximum,
                "peak_active": self.peak_active,
                "final_limit": self.limit,
                "transitions": [asdict(item) for item in self.transitions],
            }
