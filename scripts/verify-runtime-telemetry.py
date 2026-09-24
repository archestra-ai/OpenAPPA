#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["opentelemetry-proto==1.37.0"]
# ///
"""Verify the real runtime's value-safe OTLP surface with its native hook client."""

from __future__ import annotations

import http.server
import json
import os
import pathlib
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field

from opentelemetry.proto.collector.logs.v1.logs_service_pb2 import (
    ExportLogsServiceRequest,
)
from opentelemetry.proto.collector.metrics.v1.metrics_service_pb2 import (
    ExportMetricsServiceRequest,
)
from opentelemetry.proto.collector.trace.v1.trace_service_pb2 import (
    ExportTraceServiceRequest,
)

MARKERS = {
    "argument-marker-7e78f8",
    "result-marker-52c176",
    "prompt-marker-df439b",
}
EXPECTED_METRICS = {
    "appa.policy.decisions",
    "appa.policy.check.duration",
    "appa.external.calls",
    "appa.external.call.duration",
    "appa.remedy.attempts",
    "appa.remedy.duration",
    "appa.yell.reports",
    "appa.runtime.failures",
    "appa.runtime.uptime",
}
METRIC_ATTRIBUTES = {
    "appa.policy.decisions": {"outcome"},
    "appa.policy.check.duration": {"outcome"},
    "appa.external.calls": {"role", "outcome"},
    "appa.external.call.duration": {"role", "outcome"},
    "appa.remedy.attempts": {"outcome"},
    "appa.remedy.duration": {"outcome"},
    "appa.yell.reports": {"source"},
    "appa.runtime.failures": {"component", "error_type"},
    "appa.runtime.uptime": set(),
}


def scalar(value):
    selected = value.WhichOneof("value")
    return getattr(value, selected) if selected else None


def attributes(values):
    return {item.key: scalar(item.value) for item in values}


@dataclass
class Captured:
    lock: threading.Lock = field(default_factory=threading.Lock)
    requests: list[tuple[str, dict[str, str], bytes]] = field(default_factory=list)
    annotation_requests: list[dict] = field(default_factory=list)


class Receiver(http.server.BaseHTTPRequestHandler):
    state: Captured

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", "0")))
        if self.path == "/approve":
            payload = b'{"version":1,"answer":{"ruling":"approve"}}'
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        if self.path in ("/annotate-ok", "/annotate-fail"):
            with self.state.lock:
                self.state.annotation_requests.append(json.loads(body))
            if self.path == "/annotate-fail":
                self.send_response(503)
                payload = b"fixture annotation failure"
                self.send_header("content-type", "text/plain")
            else:
                self.send_response(200)
                payload = json.dumps(
                    {
                        "version": 1,
                        "answer": {
                            "delta": {},
                            "requires": {"history": [], "attention": []},
                            "emits": [],
                        },
                    }
                ).encode()
                self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        with self.state.lock:
            self.state.requests.append((self.path, dict(self.headers.items()), body))
        self.send_response(200)
        self.send_header("content-type", "application/x-protobuf")
        self.send_header("content-length", "0")
        self.end_headers()

    def log_message(self, *_):
        pass


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def clean_env(**updates: str) -> dict[str, str]:
    env = {
        key: value for key, value in os.environ.items() if not key.startswith("OTEL_")
    }
    env.update(updates)
    return env


def wait_healthy(process: subprocess.Popen, url: str, log: pathlib.Path) -> None:
    for _ in range(200):
        try:
            with urllib.request.urlopen(url + "/health", timeout=0.2) as response:
                if response.read() == b"ok":
                    return
        except OSError:
            if process.poll() is not None:
                raise AssertionError(
                    f"runtime exited before health check:\n{log.read_text()}"
                )
            time.sleep(0.05)
    raise AssertionError(f"runtime never became healthy:\n{log.read_text()}")


def hook(
    binary: str, env: dict[str, str], event: dict, expected: str | None = None
) -> dict:
    completed = subprocess.run(
        [binary, "hook"],
        env=env,
        input=json.dumps(event),
        text=True,
        capture_output=True,
        timeout=15,
        check=False,
    )
    expected_code = 2 if expected == "refuse" else 0
    assert completed.returncode == expected_code, (
        event,
        completed.returncode,
        completed.stdout,
        completed.stderr,
    )
    answer = json.loads(completed.stdout or "{}")
    if expected not in (None, "refuse"):
        decision = answer.get("hookSpecificOutput", {}).get("permissionDecision")
        assert decision == expected, (event, answer)
    return answer


def direct_hook(url: str, body: bytes) -> tuple[int, dict]:
    request = urllib.request.Request(
        url + "/hook", data=body, headers={"content-type": "application/json"}
    )
    try:
        with urllib.request.urlopen(request, timeout=2) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read())


def mcp_call(url: str, name: str, arguments: dict) -> dict:
    request = urllib.request.Request(
        url + "/mcp",
        data=json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": name,
                    "arguments": arguments,
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                        "io.modelcontextprotocol/clientCapabilities": {},
                    },
                },
            }
        ).encode(),
        headers={
            "content-type": "application/json",
            "accept": "application/json, text/event-stream",
            "MCP-Protocol-Version": "2026-07-28",
            "Mcp-Method": "tools/call",
            "Mcp-Name": name,
        },
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        body = response.read().decode()
        message = (
            json.loads(body)
            if body.startswith("{")
            else json.loads(
                next(
                    line[6:] for line in body.splitlines() if line.startswith("data: ")
                )
            )
        )
    assert "error" not in message, message
    return message["result"]


def event(name: str, **values) -> dict:
    return {"hook_event_name": name, "session_id": "telemetry-session", **values}


def exercise(
    binary: str,
    root: pathlib.Path,
    env: dict[str, str],
    *,
    graceful: bool,
    captured: Captured | None = None,
) -> dict[str, str]:
    port = free_port()
    url = f"http://127.0.0.1:{port}"
    env.update(
        APPA_GATE="1", APPA_RUNTIME_URL=url, APPA_YELL_ENDPOINT="http://127.0.0.1:1"
    )
    log_path = root / "runtime.log"
    with log_path.open("w+") as log:
        runtime = subprocess.Popen(
            [
                binary,
                "runtime",
                "--config",
                str(root / "appa.toml"),
                "--db",
                str(root / "appa.db"),
                "--listen",
                f"127.0.0.1:{port}",
            ],
            env=env,
            stdout=log,
            stderr=log,
        )
        try:
            wait_healthy(runtime, url, log_path)
            hook(binary, env, event("SessionStart"))
            hook(binary, env, event("UserPromptSubmit", prompt="prompt-marker-df439b"))
            answers = {}
            answers["allowed"] = hook(
                binary,
                env,
                event(
                    "PreToolUse",
                    tool_name="Allowed",
                    tool_use_id="call-allowed-private-id",
                    tool_input={"value": "argument-marker-7e78f8"},
                ),
                "allow",
            )
            hook(
                binary,
                env,
                event(
                    "PostToolUse",
                    tool_name="Allowed",
                    tool_use_id="call-allowed-private-id",
                    tool_input={"value": "argument-marker-7e78f8"},
                    tool_response={"content": "result-marker-52c176"},
                ),
            )
            answers["denied"] = hook(
                binary,
                env,
                event(
                    "PreToolUse",
                    tool_name="Blocked",
                    tool_use_id="call-denied-private-id",
                    tool_input={},
                ),
                "deny",
            )
            answers["annotated"] = hook(
                binary,
                env,
                event(
                    "PreToolUse",
                    tool_name="Annotated",
                    tool_use_id="call-annotated-private-id",
                    tool_input={},
                ),
                "allow",
            )
            # Both failures render as host denials, but neither is a policy denial.
            answers["undeclared"] = hook(
                binary,
                env,
                event(
                    "PreToolUse",
                    tool_name="NotDeclared",
                    tool_use_id="call-undeclared-private-id",
                    tool_input={},
                ),
                "refuse",
            )
            answers["annotation_failure"] = hook(
                binary,
                env,
                event(
                    "PreToolUse",
                    tool_name="AnnotationFailure",
                    tool_use_id="call-failed-private-id",
                    tool_input={},
                ),
                "refuse",
            )
            status, denied = direct_hook(
                url,
                json.dumps(
                    {
                        "protocol": 1,
                        "adapter": "claude-code",
                        "event": "tool_call",
                        "root_id": "telemetry-session",
                        "tool": "NeedsReview",
                        "arguments": {},
                        "call_id": "review-call",
                    }
                ).encode(),
            )
            assert status == 200 and denied["decision"] == "deny_call", denied
            offer = denied["offers"][0]["offer_id"]
            remedy_args = {"offer_id": offer}
            hook(
                binary,
                env,
                event(
                    "PreToolUse",
                    tool_name="mcp__appa__execute_remedy_plan",
                    tool_input=remedy_args,
                ),
            )
            remedy = mcp_call(url, "execute_remedy_plan", remedy_args)
            assert not remedy.get("isError", False), remedy
            assert "Authorized" in json.dumps(remedy), remedy

            yell_args = {
                "message": "Telemetry verification report",
                "with_trajectory": True,
            }
            # An unvouched report must emit no report event.
            unvouched = mcp_call(url, "yell", yell_args)
            assert unvouched.get("isError"), unvouched
            hook(
                binary,
                env,
                event(
                    "PreToolUse",
                    tool_name="mcp__appa__yell",
                    tool_use_id="call-yell",
                    tool_input=yell_args,
                ),
                "allow",
            )
            # The receiver is unavailable. The approved report event must still export.
            reported = mcp_call(url, "yell", yell_args)
            assert reported.get("isError"), reported
            unreadable_status, _ = direct_hook(url, b"{")
            malformed_status, _ = direct_hook(
                url, json.dumps(event("PreToolUse")).encode()
            )
            assert unreadable_status == 400
            assert malformed_status == 409
            if captured is not None:
                with captured.lock:
                    assert not captured.requests, (
                        "batch telemetry exported before shutdown"
                    )
            return {
                key: value.get("hookSpecificOutput", {}).get(
                    "permissionDecision", "refuse"
                )
                for key, value in answers.items()
            }
        finally:
            if runtime.poll() is None:
                runtime.send_signal(signal.SIGTERM)
                try:
                    code = runtime.wait(timeout=20 if graceful else 5)
                    if graceful:
                        assert code == 0, log_path.read_text()
                except subprocess.TimeoutExpired:
                    runtime.kill()
                    runtime.wait(timeout=5)
                    raise AssertionError(
                        f"runtime did not stop within its deadline:\n{log_path.read_text()}"
                    )


def points(metric):
    data = getattr(metric, metric.WhichOneof("data"))
    return list(data.data_points)


def point_value(point) -> float:
    selected = point.WhichOneof("value")
    return float(getattr(point, selected)) if selected else 0.0


def decode(captured: Captured):
    spans, logs, metrics = [], [], []
    resources = []
    with captured.lock:
        requests = list(captured.requests)
    assert requests, "shutdown exported no OTLP requests"
    for path, headers, body in requests:
        lowered_headers = {key.lower(): value for key, value in headers.items()}
        assert lowered_headers.get("x-appa-verification") == "header-propagated", (
            path,
            headers,
        )
        if path == "/v1/traces":
            message = ExportTraceServiceRequest.FromString(body)
            resources.extend(item.resource for item in message.resource_spans)
            spans.extend(
                span
                for item in message.resource_spans
                for scope in item.scope_spans
                for span in scope.spans
            )
        elif path == "/v1/logs":
            message = ExportLogsServiceRequest.FromString(body)
            resources.extend(item.resource for item in message.resource_logs)
            logs.extend(
                log
                for item in message.resource_logs
                for scope in item.scope_logs
                for log in scope.log_records
            )
        elif path == "/v1/metrics":
            message = ExportMetricsServiceRequest.FromString(body)
            resources.extend(item.resource for item in message.resource_metrics)
            metrics.extend(
                metric
                for item in message.resource_metrics
                for scope in item.scope_metrics
                for metric in scope.metrics
            )
        else:
            raise AssertionError(f"unexpected collector path {path}")
    return requests, resources, spans, logs, metrics


def verify_telemetry(captured: Captured) -> None:
    requests, resources, spans, logs, metrics = decode(captured)
    for resource in resources:
        attrs = attributes(resource.attributes)
        assert attrs["service.name"] == "appa-telemetry-verification"
        assert attrs["verification.resource"] == "propagated"

    metric_names = {metric.name for metric in metrics}
    assert EXPECTED_METRICS <= metric_names, (
        EXPECTED_METRICS - metric_names,
        metric_names,
    )
    by_name = {metric.name: metric for metric in metrics}
    for metric in metrics:
        assert metric.name in METRIC_ATTRIBUTES, f"unexpected metric {metric.name}"
        for point in points(metric):
            keys = set(attributes(point.attributes))
            assert keys <= METRIC_ATTRIBUTES[metric.name], (metric.name, keys)
            assert not any(
                token in key
                for key in keys
                for token in ("tool", "trajectory", "run", "call", "session")
            )

    decision_points = points(by_name["appa.policy.decisions"])
    decision_totals = {
        attributes(point.attributes)["outcome"]: point_value(point)
        for point in decision_points
    }
    assert decision_totals == {"allowed": 3.0, "denied": 2.0}, decision_totals
    durations = {
        attributes(point.attributes)["outcome"]
        for point in points(by_name["appa.policy.check.duration"])
    }
    assert durations == {"allowed", "denied", "error"}, durations
    failures = {
        (
            attributes(point.attributes)["component"],
            attributes(point.attributes)["error_type"],
        ): point_value(point)
        for point in points(by_name["appa.runtime.failures"])
    }
    assert failures.get(("policy_check", "undeclared_tool"), 0) == 1, failures
    assert failures.get(("policy_check", "annotation_refused"), 0) == 1, failures
    assert [
        (attributes(point.attributes), point_value(point))
        for point in points(by_name["appa.remedy.attempts"])
    ] == [({"outcome": "executed"}, 1.0)]
    assert [
        (attributes(point.attributes), point_value(point))
        for point in points(by_name["appa.yell.reports"])
    ] == [({"source": "agent"}, 1.0)]

    span_attrs = [(span, attributes(span.attributes)) for span in spans]
    policy_spans = [
        (span, attrs) for span, attrs in span_attrs if span.name == "appa.policy.check"
    ]
    assert {attrs.get("appa.outcome") for _, attrs in policy_spans} >= {
        "allowed",
        "denied",
        "error",
    }
    assert any(attrs.get("appa.policy.id") for _, attrs in policy_spans), (
        "policy identity was not recorded"
    )
    assert any(
        "attention" in attrs.get("appa.policy.gaps", "") for _, attrs in policy_spans
    ), "block gaps were not recorded"
    external_spans = [
        (span, attrs) for span, attrs in span_attrs if span.name == "appa.external.call"
    ]
    assert len(external_spans) >= 2, (
        "successful and failed annotation calls were not traced"
    )
    assert any(
        child.parent_span_id == parent.span_id and child.trace_id == parent.trace_id
        for child, _ in external_spans
        for parent, _ in policy_spans
    ), "external call was not nested under its policy check"

    log_attrs = [(log, attributes(log.attributes)) for log in logs]
    assert log_attrs, "no OTLP logs were exported"
    assert all(attrs.get("appa.event.name") for _, attrs in log_attrs), (
        "a non-appa_telemetry log escaped"
    )
    reports = [
        attrs
        for _, attrs in log_attrs
        if attrs["appa.event.name"] == "appa.yell.report"
    ]
    assert (
        len(reports) == 1
        and reports[0]["appa.report.message"] == "Telemetry verification report"
    ), reports
    assert reports[0]["appa.report.id"] and reports[0]["appa.trajectory.root"]
    assert any(
        log.trace_id == span.trace_id
        and log.span_id == span.span_id
        and attrs.get("appa.event.name") == "appa.policy.decision"
        for log, attrs in log_attrs
        for span, _ in policy_spans
    ), "policy log did not carry its exported span's trace/span IDs"

    rendered = b"\n".join(body for _, _, body in requests).decode("latin1")
    for marker in MARKERS:
        assert marker not in rendered, f"value marker escaped in telemetry: {marker}"
    assert "gen_ai.tool.call.arguments" not in rendered
    with captured.lock:
        assert len(captured.annotation_requests) == 2, captured.annotation_requests


def write_config(path: pathlib.Path, fixture_url: str) -> None:
    path.write_text(
        f'''[policy]
version = 2

[[policy.tool]]
name = "host/claude-code/Allowed"
delta = {{}}

[[policy.tool]]
name = "host/claude-code/Blocked"
requires = {{ attention = ["blocked"] }}
delta = {{}}

[[policy.annotator]]
name = "telemetry-ok"

[[policy.tool]]
name = "host/claude-code/Annotated"
annotator = "telemetry-ok"

[[policy.annotator]]
name = "telemetry-fail"

[[policy.tool]]
name = "host/claude-code/AnnotationFailure"
annotator = "telemetry-fail"

[[policy.tool]]
name = "host/claude-code/NeedsReview"
delta = {{}}
requires = {{ attention = ["signoff"] }}

[[policy.authority]]
name = "operator"
[policy.authority.permits]
attention = ["signoff"]

[[policy.tool]]
name = "mcp/appa/yell"
delta = {{}}

[reporting]
agent_yell = true

[externals]
timeout_ms = 1000
max_body_bytes = 65536

[externals.annotators.telemetry-ok]
url = "{fixture_url}/annotate-ok"

[externals.annotators.telemetry-fail]
url = "{fixture_url}/annotate-fail"

[externals.authorities.operator]
url = "{fixture_url}/approve"
'''
    )


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: uv run {pathlib.Path(__file__).name} /path/to/appa")
    binary = str(pathlib.Path(sys.argv[1]).resolve())
    if not os.path.isfile(binary) or not os.access(binary, os.X_OK):
        raise SystemExit(f"runtime binary is not executable: {binary}")

    captured = Captured()
    Receiver.state = captured
    collector = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Receiver)
    thread = threading.Thread(target=collector.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(
            prefix="appa-telemetry-verification-"
        ) as directory:
            root = pathlib.Path(directory)
            enabled_env = clean_env(
                OTEL_EXPORTER_OTLP_ENDPOINT=f"http://127.0.0.1:{collector.server_port}",
                OTEL_EXPORTER_OTLP_PROTOCOL="http/protobuf",
                OTEL_EXPORTER_OTLP_HEADERS="x-appa-verification=header-propagated",
                OTEL_EXPORTER_OTLP_TIMEOUT="500",
                OTEL_METRIC_EXPORT_INTERVAL="60000",
                OTEL_BSP_SCHEDULE_DELAY="60000",
                OTEL_BLRP_SCHEDULE_DELAY="60000",
                OTEL_SERVICE_NAME="appa-telemetry-verification",
                OTEL_RESOURCE_ATTRIBUTES="verification.resource=propagated,archestra.agent_run.id=private-run-id",
                OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT="true",
                RUST_LOG="trace",
            )
            # Each run needs a private database and log, while sharing the same fixture config.
            enabled_root = root / "enabled"
            enabled_root.mkdir()
            write_config(
                enabled_root / "appa.toml", f"http://127.0.0.1:{collector.server_port}"
            )
            enabled = exercise(
                binary, enabled_root, enabled_env, graceful=True, captured=captured
            )
            verify_telemetry(captured)

            disabled_root = root / "disabled"
            disabled_root.mkdir()
            write_config(
                disabled_root / "appa.toml", f"http://127.0.0.1:{collector.server_port}"
            )
            before = len(captured.requests)
            disabled = exercise(
                binary,
                disabled_root,
                clean_env(
                    OTEL_SDK_DISABLED="true",
                    OTEL_EXPORTER_OTLP_ENDPOINT=f"http://127.0.0.1:{collector.server_port}",
                    RUST_LOG="trace",
                ),
                graceful=False,
            )
            assert len(captured.requests) == before, (
                "OTEL_SDK_DISABLED runtime exported telemetry"
            )
            assert disabled == enabled, (enabled, disabled)

            unconfigured_root = root / "unconfigured"
            unconfigured_root.mkdir()
            write_config(
                unconfigured_root / "appa.toml",
                f"http://127.0.0.1:{collector.server_port}",
            )
            unconfigured = exercise(
                binary, unconfigured_root, clean_env(RUST_LOG="trace"), graceful=False
            )
            assert len(captured.requests) == before, (
                "unconfigured runtime exported telemetry"
            )
            assert unconfigured == enabled, (enabled, unconfigured)

            unavailable_root = root / "unavailable"
            unavailable_root.mkdir()
            write_config(
                unavailable_root / "appa.toml",
                f"http://127.0.0.1:{collector.server_port}",
            )
            unavailable = exercise(
                binary,
                unavailable_root,
                clean_env(
                    OTEL_EXPORTER_OTLP_ENDPOINT=f"http://127.0.0.1:{free_port()}",
                    OTEL_EXPORTER_OTLP_PROTOCOL="http/protobuf",
                    OTEL_EXPORTER_OTLP_TIMEOUT="200",
                    OTEL_METRIC_EXPORT_INTERVAL="200",
                    OTEL_BSP_SCHEDULE_DELAY="100",
                    OTEL_BLRP_SCHEDULE_DELAY="100",
                    RUST_LOG="trace",
                ),
                graceful=True,
            )
            assert unavailable == enabled, (enabled, unavailable)
    finally:
        collector.shutdown()
        collector.server_close()
        thread.join(timeout=5)
    print(
        "real runtime OTLP verification passed: decisions, failures, remedies, agent reports, correlation, value safety, resources, headers, shutdown, disabled, unconfigured and unavailable export"
    )


if __name__ == "__main__":
    main()
