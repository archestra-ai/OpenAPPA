"""Capability-scoped loopback execution server for AgentDojo tools."""

import copy
import json
import logging
import secrets
import threading
from collections.abc import Callable
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Literal, cast

from agentdojo.functions_runtime import FunctionReturnType, FunctionsRuntime, TaskEnvironment

PROTOCOL_IDENTITY = "agentdojo-loopback-v2"
MAX_REQUEST_BODY_BYTES = 1024 * 1024
MAX_RESULT_BODY_BYTES = 256 * 1024
FORMATTER_FAILURE_STATUS = 460

logger = logging.getLogger(__name__)

ExecutionOutcome = Literal["success", "success_without_value", "indeterminate"]
OutputFormatter = Callable[[FunctionReturnType], str]


@dataclass(frozen=True)
class ExecutionRecord:
    tool: str
    arguments: dict[str, object]
    outcome: ExecutionOutcome


@dataclass(frozen=True)
class _Episode:
    token: str
    runtime: FunctionsRuntime
    env: TaskEnvironment
    output_formatter: OutputFormatter
    tools: frozenset[str]


class _BridgeServer(HTTPServer):
    def __init__(self, bridge: "ToolBridge") -> None:
        self.bridge = bridge
        super().__init__(("127.0.0.1", 0), _BridgeRequestHandler)


class _BridgeRequestHandler(BaseHTTPRequestHandler):
    server: _BridgeServer

    def setup(self) -> None:
        super().setup()
        self.connection.settimeout(30)

    def do_POST(self) -> None:
        self.server.bridge._handle(self)

    def log_message(self, format: str, *args: object) -> None:
        return


class ToolBridge:
    """One serialized HTTP capability endpoint with one active episode slot."""

    def __init__(self) -> None:
        self._episode_lock = threading.Lock()
        self._episode: _Episode | None = None
        self._execution_log: list[ExecutionRecord] = []
        self._server = _BridgeServer(self)
        self._thread = threading.Thread(target=self._server.serve_forever, name="appa-dojo-tool-bridge", daemon=True)
        self._thread.start()
        self._closed = False

    @property
    def execution_log(self) -> tuple[ExecutionRecord, ...]:
        with self._episode_lock:
            return tuple(copy.deepcopy(self._execution_log))

    def open_episode(
        self,
        runtime: FunctionsRuntime,
        env: TaskEnvironment,
        output_formatter: OutputFormatter,
        tools: set[str],
    ) -> str:
        if self._closed:
            raise RuntimeError("the tool bridge is closed")
        token = secrets.token_urlsafe(32)
        with self._episode_lock:
            self._execution_log.clear()
            self._episode = _Episode(
                token=token,
                runtime=runtime,
                env=env,
                output_formatter=output_formatter,
                tools=frozenset(tools),
            )
        host, port = self._server.server_address
        if host != "127.0.0.1":
            raise RuntimeError("the tool bridge did not bind literal loopback")
        return f"http://127.0.0.1:{port}/invoke/{token}"

    def close_episode(self) -> None:
        with self._episode_lock:
            self._episode = None
            self._execution_log.clear()

    def close(self) -> None:
        if self._closed:
            return
        self.close_episode()
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)
        self._closed = True
        if self._thread.is_alive():
            raise RuntimeError("the tool bridge thread did not stop")

    def __enter__(self) -> "ToolBridge":
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> None:
        self.close()

    def _handle(self, handler: _BridgeRequestHandler) -> None:
        with self._episode_lock:
            episode = self._episode
            if episode is None or handler.path != f"/invoke/{episode.token}":
                self._respond(handler, 404)
                return

            request = self._read_request(handler)
            if request is None:
                return
            tool, arguments = request
            if tool not in episode.tools:
                self._respond(handler, 422)
                return

            try:
                tool_result, error = episode.runtime.run_function(episode.env, tool, arguments)
            except Exception:
                self._record(tool, arguments, "indeterminate")
                logger.exception("AgentDojo tool %s raised outside its result envelope", tool, exc_info=False)
                self._respond(handler, 500)
                return
            if error is not None:
                self._record(tool, arguments, "indeterminate")
                self._respond(handler, 500)
                return
            try:
                content = episode.output_formatter(tool_result)
                if not isinstance(content, str):
                    raise TypeError("tool output formatter must return str")
                if len(content) > MAX_RESULT_BODY_BYTES:
                    self._record(tool, arguments, "success_without_value")
                    self._respond(handler, 413)
                    return
                body = content.encode("utf-8")
            except Exception:
                self._record(tool, arguments, "success_without_value")
                logger.exception("could not format the result of AgentDojo tool %s", tool, exc_info=False)
                self._respond(handler, FORMATTER_FAILURE_STATUS)
                return
            if len(body) > MAX_RESULT_BODY_BYTES:
                self._record(tool, arguments, "success_without_value")
                self._respond(handler, 413)
                return

            self._record(tool, arguments, "success")
            self._respond(handler, 200, body)

    def _read_request(self, handler: _BridgeRequestHandler) -> tuple[str, dict[str, object]] | None:
        content_length = handler.headers.get("Content-Length")
        try:
            length = int(content_length) if content_length is not None else -1
        except ValueError:
            length = -1
        if length < 0:
            self._respond(handler, 422)
            return None
        if length > MAX_REQUEST_BODY_BYTES:
            handler.close_connection = True
            self._respond(handler, 413)
            return None
        try:
            body = handler.rfile.read(length)
        except OSError:
            self._respond(handler, 400)
            return None
        if len(body) != length:
            self._respond(handler, 400)
            return None
        try:
            value = json.loads(body)
        except (UnicodeDecodeError, json.JSONDecodeError):
            self._respond(handler, 422)
            return None
        if not isinstance(value, dict) or set(value) != {"tool", "arguments"}:
            self._respond(handler, 422)
            return None
        tool = value["tool"]
        arguments = value["arguments"]
        if not isinstance(tool, str) or not isinstance(arguments, dict):
            self._respond(handler, 422)
            return None
        return tool, cast(dict[str, object], arguments)

    def _record(self, tool: str, arguments: dict[str, object], outcome: ExecutionOutcome) -> None:
        self._execution_log.append(
            ExecutionRecord(
                tool=tool,
                arguments=copy.deepcopy(arguments),
                outcome=outcome,
            )
        )

    @staticmethod
    def _respond(handler: _BridgeRequestHandler, status: int, body: bytes = b"") -> None:
        try:
            handler.send_response(status)
            handler.send_header("Content-Length", str(len(body)))
            handler.send_header("Content-Type", "text/plain; charset=utf-8")
            handler.end_headers()
            if body:
                handler.wfile.write(body)
        except OSError:
            pass
