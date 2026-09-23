"""Executor-compatible adaptive admission over a fixed thread pool."""

from __future__ import annotations

import queue
import threading
from collections.abc import Callable
from concurrent.futures import Future, ThreadPoolExecutor
from pathlib import Path
from typing import TypeVar

from .controller import AdaptiveConcurrency

T = TypeVar("T")


def _looks_throttled(error: BaseException) -> bool:
    text = str(error).lower()
    return "429" in text or "rate limit" in text or "rate_limit" in text


class AdaptiveThreadPoolExecutor:
    """Accept Executor submissions eagerly while adapting actual admissions."""

    def __init__(
        self,
        max_workers: int | None = None,
        *,
        history_path: Path | None = None,
        clean_result: Callable[[object], bool] | None = None,
        throttled_result: Callable[[object], bool] | None = None,
        controller: AdaptiveConcurrency | None = None,
    ) -> None:
        maximum = max_workers or 1
        self.controller = controller or AdaptiveConcurrency(maximum, history_path=history_path)
        self._clean_result = clean_result or (lambda _result: True)
        self._throttled_result = throttled_result or (lambda _result: False)
        self._pool = ThreadPoolExecutor(max_workers=maximum)
        self._pending: queue.Queue[tuple[Future, Callable, tuple, dict] | None] = queue.Queue()
        self._stop = threading.Event()
        self._dispatcher = threading.Thread(target=self._dispatch, name="benchmark-admission", daemon=True)
        self._monitor = threading.Thread(target=self._monitor_memory, name="benchmark-memory", daemon=True)
        self._dispatcher.start()
        self._monitor.start()

    def submit(self, fn: Callable[..., T], /, *args, **kwargs) -> Future[T]:
        if self._stop.is_set():
            raise RuntimeError("cannot schedule new futures after shutdown")
        outer: Future[T] = Future()
        self._pending.put((outer, fn, args, kwargs))
        return outer

    def _dispatch(self) -> None:
        while True:
            item = self._pending.get()
            if item is None:
                return
            outer, fn, args, kwargs = item
            if not outer.set_running_or_notify_cancel():
                continue
            if not self.controller.wait_for_slot(self._stop):
                outer.cancel()
                continue
            self.controller.started()
            inner = self._pool.submit(fn, *args, **kwargs)

            def finish(done: Future, destination: Future = outer) -> None:
                error = done.exception()
                if error is None:
                    result = done.result()
                    destination.set_result(result)
                    self.controller.completed(
                        clean=self._clean_result(result),
                        throttled=self._throttled_result(result),
                    )
                else:
                    destination.set_exception(error)
                    self.controller.completed(clean=False, throttled=_looks_throttled(error))

            inner.add_done_callback(finish)

    def _monitor_memory(self) -> None:
        while not self._stop.wait(1.0):
            self.controller.observe()

    def shutdown(self, wait: bool = True, *, cancel_futures: bool = False) -> None:
        self._stop.set()
        self.controller.wake()
        if cancel_futures:
            while True:
                try:
                    item = self._pending.get_nowait()
                except queue.Empty:
                    break
                if item is not None:
                    item[0].cancel()
        self._pending.put(None)
        if wait:
            self._dispatcher.join()
        self._pool.shutdown(wait=wait, cancel_futures=cancel_futures)

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> None:
        self.shutdown(wait=True)
