#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["opentelemetry-proto==1.37.0"]
# ///
"""Exercise the real runtime and native hook client against a local OTLP receiver."""

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
import urllib.request
import urllib.error
from opentelemetry.proto.collector.trace.v1.trace_service_pb2 import (
    ExportTraceServiceRequest,
)
from opentelemetry.proto.collector.logs.v1.logs_service_pb2 import (
    ExportLogsServiceRequest,
)
from opentelemetry.proto.collector.metrics.v1.metrics_service_pb2 import (
    ExportMetricsServiceRequest,
)

binary = str(pathlib.Path(sys.argv[1]).resolve())
requests = []


class Receiver(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        requests.append(
            (self.path, self.rfile.read(int(self.headers["content-length"])))
        )
        self.send_response(200)
        self.send_header("content-type", "application/x-protobuf")
        self.send_header("content-length", "0")
        self.end_headers()

    def log_message(self, *_):
        pass


def attributes(values):
    return {item.key: item.value.string_value for item in values}


collector = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Receiver)
thread = threading.Thread(target=collector.serve_forever, daemon=True)
thread.start()
try:
    for capture in (False, True):
        requests.clear()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            config = root / "appa.toml"
            config.write_text(
                '[policy]\nversion = 2\n[[policy.tool]]\nname = "host/claude-code/Bash"\ndelta = {}\n[externals]\ntimeout_ms = 1000\nmax_body_bytes = 4096\n'
            )
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            url = f"http://127.0.0.1:{port}"
            env = {k: v for k, v in os.environ.items() if not k.startswith("OTEL_")}
            env.update(
                OTEL_EXPORTER_OTLP_ENDPOINT=f"http://127.0.0.1:{collector.server_port}",
                OTEL_EXPORTER_OTLP_PROTOCOL="http/protobuf",
                OTEL_METRIC_EXPORT_INTERVAL="200",
                OTEL_BSP_SCHEDULE_DELAY="100",
                OTEL_BLRP_SCHEDULE_DELAY="100",
                OTEL_RESOURCE_ATTRIBUTES="archestra.agent_run.id=telemetry-run,archestra.task.id=telemetry-task",
                OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=str(capture).lower(),
                APPA_GATE="1",
                APPA_RUNTIME_URL=url,
            )
            with (root / "runtime.log").open("w+") as log:
                runtime = subprocess.Popen(
                    [
                        binary,
                        "runtime",
                        "--config",
                        str(config),
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
                    for _ in range(100):
                        try:
                            with urllib.request.urlopen(
                                url + "/health", timeout=0.2
                            ) as response:
                                assert response.read() == b"ok"
                            break
                        except OSError:
                            assert runtime.poll() is None, (
                                root / "runtime.log"
                            ).read_text()
                            time.sleep(0.05)
                    else:
                        raise AssertionError("runtime never became healthy")
                    for event in [
                        dict(
                            hook_event_name="SessionStart", session_id="telemetry-test"
                        ),
                        dict(
                            hook_event_name="PreToolUse",
                            session_id="telemetry-test",
                            tool_name="Bash",
                            tool_input=dict(command="echo telemetry-payload-marker"),
                        ),
                    ]:
                        result = subprocess.run(
                            [binary, "hook"],
                            env=env,
                            input=json.dumps(event),
                            text=True,
                            capture_output=True,
                            timeout=10,
                        )
                        assert result.returncode == 0, result.stderr
                    try:
                        urllib.request.urlopen(
                            urllib.request.Request(
                                url + "/hook",
                                data=b"{",
                                headers={"content-type": "application/json"},
                            ),
                            timeout=2,
                        )
                        raise AssertionError("malformed hook was accepted")
                    except urllib.error.HTTPError as error:
                        assert error.code == 400
                    runtime.send_signal(signal.SIGTERM)
                    assert runtime.wait(timeout=20) == 0
                finally:
                    if runtime.poll() is None:
                        runtime.kill()
                        runtime.wait()
            spans = []
            logs = []
            metrics = []
            for path, data in requests:
                if path == "/v1/traces":
                    message = ExportTraceServiceRequest.FromString(data)
                    for resource in message.resource_spans:
                        assert (
                            attributes(resource.resource.attributes)[
                                "archestra.task.id"
                            ]
                            == "telemetry-task"
                        )
                        spans.extend(
                            span
                            for scope in resource.scope_spans
                            for span in scope.spans
                        )
                elif path == "/v1/logs":
                    message = ExportLogsServiceRequest.FromString(data)
                    logs.extend(
                        log
                        for resource in message.resource_logs
                        for scope in resource.scope_logs
                        for log in scope.log_records
                    )
                elif path == "/v1/metrics":
                    message = ExportMetricsServiceRequest.FromString(data)
                    metrics.extend(
                        metric
                        for resource in message.resource_metrics
                        for scope in resource.scope_metrics
                        for metric in scope.metrics
                    )
            call = next(
                span
                for span in spans
                if attributes(span.attributes).get("appa.hook.event") == "tool_call"
            )
            assert attributes(call.attributes)["appa.decision"] == "allow"
            assert any(
                attributes(span.attributes).get("appa.hook.event") == "parse"
                and attributes(span.attributes).get("appa.decision") == "refuse"
                for span in spans
            )
            assert ("telemetry-payload-marker" in str(call)) == capture
            assert any(
                log.trace_id == call.trace_id and log.span_id == call.span_id
                for log in logs
            )
            assert all(
                "telemetry-payload-marker" not in str(log)
                and "gen_ai.tool.call.arguments" not in str(log)
                for log in logs
            )
            assert {
                "appa.runtime.hook.requests",
                "appa.runtime.hook.duration",
                "appa.runtime.uptime",
            } <= {metric.name for metric in metrics}
            for metric in metrics:
                data = getattr(metric, metric.WhichOneof("data"))
                for point in data.data_points:
                    assert all(
                        item.key in ("appa.hook.event", "appa.decision")
                        for item in point.attributes
                    )
            print(
                f"OTLP traces, correlated logs, bounded metrics and shutdown flush passed (capture={capture})"
            )
finally:
    collector.shutdown()
    collector.server_close()
