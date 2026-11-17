"""Measure a release daemon on one growing SQLite trajectory with synthetic tool results."""

import argparse
from collections import deque
from contextlib import closing, contextmanager
from http.client import HTTPConnection
import json
import math
import os
from pathlib import Path
from queue import Empty, Queue
import re
import socket
from statistics import median
import subprocess
from tempfile import TemporaryDirectory
from threading import Thread
from time import perf_counter_ns
from urllib.parse import urlsplit

POLICY = """[policy]
version = 2

[[policy.tool]]
name = "host/claude-code/Bash"
"""
DECODED = re.compile(rb"decoded_batches=(\d+)")


@contextmanager
def runtime(binary, directory, diagnostics, trace):
    config = directory / "appa.toml"
    config.write_text(POLICY, encoding="utf-8")
    env = {key: value for key, value in os.environ.items() if not key.startswith(("APPA_", "OTEL_"))}
    env["NO_COLOR"] = "1"
    env["RUST_LOG"] = "warn,appa_eventlog::sqlite=trace" if trace else "warn"
    with diagnostics.open("wb") as logs, subprocess.Popen(
        [binary, "runtime", "--config", config, "--db", directory / "appa.db", "--listen", "127.0.0.1:0"],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=logs, env=env,
    ) as process:
        try:
            ready = Queue()
            Thread(target=lambda: ready.put(process.stdout.readline()), daemon=True).start()
            try:
                url = urlsplit(ready.get(timeout=15).decode().strip())
            except Empty:
                raise RuntimeError(f"runtime did not bind within 15 seconds\n{diagnostics.read_text()}") from None
            if url.hostname != "127.0.0.1" or url.port is None:
                raise RuntimeError(f"runtime did not report a loopback endpoint\n{diagnostics.read_text()}")
            yield url
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


def hook(connection, event, expected, **fields):
    request = {"protocol": 1, "adapter": "claude-code", "root_id": "long-trajectory", "event": event, **fields}
    body = json.dumps(request).encode()
    start = perf_counter_ns()
    connection.request("POST", "/hook", body, {"Content-Type": "application/json"})
    response = connection.getresponse()
    answer = response.read()
    elapsed_ms = (perf_counter_ns() - start) / 1e6
    if response.status != 200 or json.loads(answer)["decision"] != expected:
        raise RuntimeError(f"{event} expected {expected}, got HTTP {response.status} {answer!r}")
    return elapsed_ms


def measure(connection, logs, calls, trace, check_decodes):
    hook(connection, "session_start", "ack")
    if trace:
        logs.seek(0, os.SEEK_END)
    window = deque(maxlen=50)
    checkpoints = {10, 100, 500, 1000, 2500, 5000, calls}
    for number in range(1, calls + 1):
        fields = {"tool": "Bash", "arguments": {"command": "pwd"}, "call_id": f"call-{number}"}
        before = hook(connection, "tool_call", "allow_call", **fields)
        after = hook(
            connection, "tool_result", "ack", **fields, outcome={"status": "success", "body": {"stdout": "fixture"}}
        )
        decoded = None
        if trace:
            reads = DECODED.findall(logs.read())
            if not reads:
                raise RuntimeError("SQLite trace missing for the tool pair. Use the current build or --timings-only.")
            decoded = sum(map(int, reads))
        window.append((before + after, decoded))
        if number in checkpoints:
            durations, batches = zip(*window)
            p95 = sorted(durations)[math.ceil(0.95 * len(durations)) - 1]
            result = {"calls": number, "samples": len(window), "pair_p50_ms": median(durations), "pair_p95_ms": p95}
            if trace:
                result["max_decoded_batches_per_pair"] = max(batches)
            print(json.dumps(result), flush=True)
        if check_decodes and decoded > 2:
            raise RuntimeError(f"pair {number} decoded {decoded} batches, expected at most 2")


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument(
        "binary", nargs="?", type=Path, default=Path(__file__).resolve().parents[2] / "target/release/appa",
        help="appa release executable (default: target/release/appa in this checkout)",
    )
    parser.add_argument("--calls", type=int, default=2500, help="tool-call/result pairs to run (default: 2500)")
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--timings-only", action="store_true", help="disable decode tracing for latency comparisons")
    modes.add_argument(
        "--check-decodes", action="store_true", help="fail if a warm-cache workload decodes more than two batches per pair"
    )
    args = parser.parse_args()
    if args.calls <= 0:
        parser.error("--calls must be positive")
    if not args.binary.is_file():
        parser.error(f"{args.binary} is missing. Build it with cargo build --release -p appa --bin appa.")

    trace = not args.timings_only
    try:
        with TemporaryDirectory(prefix="appa-trajectory-bench-") as tmp:
            directory = Path(tmp)
            diagnostics = directory / "runtime.log"
            with runtime(args.binary.resolve(), directory, diagnostics, trace) as url:
                with closing(HTTPConnection(url.hostname, url.port, timeout=30)) as connection, diagnostics.open(
                    "rb"
                ) as logs:
                    connection.connect()
                    connection.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                    measure(connection, logs, args.calls, trace, args.check_decodes)
    except (OSError, RuntimeError, ValueError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
