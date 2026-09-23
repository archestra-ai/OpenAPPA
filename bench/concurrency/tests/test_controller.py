from __future__ import annotations

import threading

from appa_bench_concurrency import AdaptiveConcurrency, AdaptiveThreadPoolExecutor, MemoryReading


def test_grows_on_clean_work_and_halves_on_live_pressure() -> None:
    reading = [MemoryReading(50, 100, "test")]
    controller = AdaptiveConcurrency(8, observer=lambda: reading[0])

    controller.started()
    controller.completed()
    controller.started()
    controller.completed()
    controller.started()
    controller.completed()
    assert controller.limit == 4

    reading[0] = MemoryReading(90, 100, "test")
    controller.observe()
    assert controller.limit == 2
    assert controller.transitions[-1].reason == "memory-pressure"


def test_ceiling_one_stays_serial() -> None:
    controller = AdaptiveConcurrency(1, observer=lambda: None)
    controller.started()
    controller.completed()
    assert controller.limit == 1
    assert controller.peak_active == 1


def test_pressure_and_throttle_are_recorded_at_serial_limit() -> None:
    controller = AdaptiveConcurrency(1, observer=lambda: MemoryReading(90, 100, "test"))
    controller.observe()
    controller.started()
    controller.completed(clean=False, throttled=True)
    assert [item.reason for item in controller.transitions] == [
        "initial",
        "memory-pressure",
        "provider-throttle",
    ]


def test_executor_starts_at_one_then_admits_parallel_work() -> None:
    bootstrap_release = threading.Event()
    parallel_started = threading.Barrier(3)

    def work(index: int) -> int:
        if index == 0:
            bootstrap_release.wait(timeout=2)
        else:
            parallel_started.wait(timeout=2)
        return index

    executor = AdaptiveThreadPoolExecutor(
        max_workers=2,
        controller=AdaptiveConcurrency(2, observer=lambda: None),
    )
    futures = [executor.submit(work, index) for index in range(3)]
    bootstrap_release.set()
    parallel_started.wait(timeout=2)
    assert [future.result(timeout=2) for future in futures] == [0, 1, 2]
    executor.shutdown()
    assert executor.controller.peak_active == 2
