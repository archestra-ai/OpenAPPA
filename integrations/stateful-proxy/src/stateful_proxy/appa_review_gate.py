"""Root-only authenticated-review Gate wrapper.

It preserves the proxy's Gate-shaped factory seam. A denied call is never
released from control-tool text: the runtime must complete its real MCP remedy,
then the exact original call is proposed to Gate again and must be allowed.
"""
from __future__ import annotations

import http.client
import json
import threading
from dataclasses import dataclass
from typing import Any, Mapping
from urllib.parse import urlsplit

from .appa_gate import Decision, Gate, GateError

_MISSING = object()


class ReviewGateError(GateError):
    """A review remedy did not establish a new ordinary-call admission."""


@dataclass(frozen=True)
class _SourceCall:
    raw_name: str
    arguments: str


class SourceGate:
    """Source-runtime hook facade without legacy kagent wire fields.

    The frozen runtime derives spawn itself and recognizes only
    ``appa:execute_remedy_plan`` as the control tool. This facade keeps the
    old task-local Gate untouched while preserving its JSON canonicalization and
    decision type.
    """

    def __init__(self, root_id: str, runtime_url: str) -> None:
        self._gate = Gate(root_id, runtime_url=runtime_url, trace_path=None)
        self.root_id = root_id
        self._calls: dict[str, _SourceCall] = {}
        self._started = False

    def session_start(self) -> Decision:
        if self._started:
            raise GateError("session_start was already sent")
        decision = self._gate._post("session_start")
        if decision.name != "ack":
            raise GateError("runtime did not acknowledge session_start")
        self._started = True
        return decision

    def prompt(self, text: str) -> Decision:
        self._require_started()
        decision = self._gate._post("prompt", text=text)
        if decision.name != "ack":
            raise GateError("runtime did not acknowledge prompt")
        return decision

    def before_call(self, call_id: str, name: str, args: Any) -> Decision:
        self._require_started()
        if not isinstance(call_id, str) or not call_id or call_id in self._calls:
            raise GateError("call id is empty or already open")
        raw_name = Gate._raw_name(name)
        arguments = Gate._canonical_arguments(args)
        # No client-provided `spawn`: the source runtime derives it from the
        # adapter and trusted tool identity.
        decision = self._gate._post("tool_call", tool=raw_name, arguments=json.loads(arguments))
        if decision.allowed:
            self._calls[call_id] = _SourceCall(raw_name, arguments)
        return decision

    def begin_remedy(self, offer_id: str) -> Decision:
        self._require_started()
        if not isinstance(offer_id, str) or not offer_id:
            raise ValueError("offer_id must be a non-empty string")
        # Source kagent adapter control spelling. Do not use legacy bare name.
        return self._gate._post(
            "tool_call",
            tool="appa:execute_remedy_plan",
            arguments={"offer_id": offer_id},
        )

    def after_result(self, call_id: str, *, body: Any = _MISSING, error: str | None = None, indeterminate: bool = False) -> Decision:
        self._require_started()
        call = self._calls.get(call_id)
        if call is None:
            raise GateError("no allowed call is open")
        selected = int(body is not _MISSING) + int(error is not None) + int(indeterminate)
        if selected > 1:
            raise ValueError("choose only one of body, error, or indeterminate")
        if indeterminate:
            outcome: dict[str, Any] = {"status": "indeterminate"}
        elif error is not None:
            outcome = {"status": "failure", "message": error}
        elif body is _MISSING:
            outcome = {"status": "success_without_body"}
        else:
            Gate._canonical_arguments(body)
            outcome = {"status": "success", "body": body}
        decision = self._gate._post("tool_result", tool=call.raw_name, arguments=json.loads(call.arguments), outcome=outcome)
        if decision.name in {"ack", "deliver_value", "replace_output", "block"}:
            del self._calls[call_id]
        return decision

    def turn_end(self) -> Decision:
        self._require_started()
        decision = self._gate._post("turn_end")
        if decision.name != "ack":
            raise GateError("runtime did not acknowledge turn_end")
        self._calls.clear()
        return decision

    def _require_started(self) -> None:
        if not self._started:
            raise GateError("call session_start first")


def _mcp_events(body: bytes) -> list[dict[str, Any]]:
    values: list[dict[str, Any]] = []
    for line in body.decode("utf-8").splitlines():
        if line.startswith("data: ") and line[6:].strip():
            value = json.loads(line[6:])
            if isinstance(value, dict):
                values.append(value)
    if not values:
        value = json.loads(body)
        if isinstance(value, dict):
            values.append(value)
    return values


@dataclass(frozen=True)
class McpResult:
    result: dict[str, Any]

    @property
    def failed(self) -> bool:
        return bool(self.result.get("isError"))


class RuntimeMcpClient:
    """Minimal Streamable-HTTP MCP client for the runtime control tool only."""

    def __init__(
        self,
        runtime_url: str,
        mcp_host: str = "127.0.0.1:8787",
        rpc_timeout_s: float = 2160,
    ) -> None:
        parsed = urlsplit(runtime_url)
        if parsed.scheme != "http" or not parsed.hostname or parsed.query or parsed.fragment:
            raise ValueError("runtime URL must be an absolute http URL without query or fragment")
        self.host, self.port = parsed.hostname, parsed.port or 80
        self.path = (parsed.path.rstrip("/") or "") + "/mcp"
        # The runtime's allow-list is for its Kubernetes service host, not the
        # loopback port-forward address used by this client.
        if not isinstance(mcp_host, str) or not mcp_host or ":" not in mcp_host:
            raise ValueError("MCP allowed host must be host:port")
        if not isinstance(rpc_timeout_s, (int, float)) or rpc_timeout_s <= 0 or rpc_timeout_s > 3600:
            raise ValueError("MCP RPC timeout must be between 0 and 3600 seconds")
        self.allowed_host = mcp_host
        self.rpc_timeout_s = rpc_timeout_s
        self._session_id: str | None = None
        self._next_id = 1
        self._lock = threading.Lock()

    def execute_remedy_plan(self, offer_id: str) -> McpResult:
        if not isinstance(offer_id, str) or not offer_id:
            raise ReviewGateError("engine offer id is missing")
        with self._lock:
            self._initialize_if_needed()
            result = self._request("tools/call", {"name": "execute_remedy_plan", "arguments": {"offer_id": offer_id}})
        if result.failed:
            raise ReviewGateError("runtime MCP control call failed")
        return result

    def _initialize_if_needed(self) -> None:
        if self._session_id is not None:
            return
        result, session = self._request_raw(
            "initialize",
            {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "appa-review-gate", "version": "1"},
            },
            None,
        )
        if not session or not isinstance(result.get("protocolVersion"), str):
            raise ReviewGateError("runtime MCP initialization returned no session")
        self._session_id = session
        tools = self._request("tools/list", {}).result.get("tools")
        if not isinstance(tools, list) or not any(isinstance(tool, dict) and tool.get("name") == "execute_remedy_plan" for tool in tools):
            raise ReviewGateError("runtime MCP does not advertise execute_remedy_plan")

    def _request(self, method: str, params: dict[str, Any]) -> McpResult:
        result, _ = self._request_raw(method, params, self._session_id)
        return McpResult(result)

    def _request_raw(self, method: str, params: dict[str, Any], session_id: str | None) -> tuple[dict[str, Any], str | None]:
        request_id = self._next_id
        self._next_id += 1
        payload = json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}, separators=(",", ":")).encode()
        connection = http.client.HTTPConnection(self.host, self.port, timeout=self.rpc_timeout_s)
        try:
            connection.putrequest("POST", self.path, skip_host=True)
            connection.putheader("Host", self.allowed_host)
            connection.putheader("Accept", "application/json, text/event-stream")
            connection.putheader("Content-Type", "application/json")
            connection.putheader("Content-Length", str(len(payload)))
            if session_id:
                connection.putheader("Mcp-Session-Id", session_id)
            connection.endheaders(payload)
            response = connection.getresponse()
            body = response.read()
            if response.status != 200:
                raise ReviewGateError(f"runtime MCP {method} returned HTTP {response.status}")
            events = _mcp_events(body)
            message = next((event for event in events if event.get("id") == request_id), None)
            if not isinstance(message, dict) or not isinstance(message.get("result"), dict):
                raise ReviewGateError(f"runtime MCP {method} returned no result")
            return message["result"], response.getheader("Mcp-Session-Id")
        except (OSError, UnicodeDecodeError, json.JSONDecodeError, http.client.HTTPException) as error:
            raise ReviewGateError(f"runtime MCP {method} failed") from error
        finally:
            connection.close()


class AppaReviewGate:
    """A Gate-compatible root adapter that waits on an authenticated authority."""

    def __init__(self, gate: Gate, runtime_mcp: RuntimeMcpClient) -> None:
        self._gate = gate
        self._runtime_mcp = runtime_mcp

    def session_start(self):
        return self._gate.session_start()

    def prompt(self, text: str):
        return self._gate.prompt(text)

    def after_result(self, call_id: str, *, body: Any = _MISSING, error: str | None = None, indeterminate: bool = False):
        if body is _MISSING:
            return self._gate.after_result(call_id, error=error, indeterminate=indeterminate)
        return self._gate.after_result(call_id, body=body, error=error, indeterminate=indeterminate)

    def turn_end(self):
        return self._gate.turn_end()

    def before_call(self, call_id: str, name: str, args: Any):
        first = self._gate.before_call(call_id, name, args)
        if first.allowed:
            return first
        offers = tuple(getattr(first, "offers", ()))
        if first.name != "deny_call" or len(offers) != 1:
            return first
        offer_id = offers[0].get("offer_id") if isinstance(offers[0], Mapping) else None
        if not isinstance(offer_id, str) or not offer_id:
            raise ReviewGateError("denial offered no usable engine remedy id")
        control = self._gate.begin_remedy(offer_id)
        if control.name != "pass_control":
            raise ReviewGateError("runtime did not vouch the offered control action")
        # This blocks until the runtime's configured external authority answers.
        # Its textual content is intentionally ignored as an authorization signal.
        self._runtime_mcp.execute_remedy_plan(offer_id)
        exact = self._gate.before_call(call_id, name, args)
        if not exact.allowed:
            raise ReviewGateError("review did not admit the exact original tool call")
        return exact


def factory(
    runtime_url: str,
    mcp_host: str = "127.0.0.1:8787",
    rpc_timeout_s: float = 2160,
):
    """Return the explicit GateMediator-compatible root factory."""
    def create(root_id: str) -> AppaReviewGate:
        return AppaReviewGate(SourceGate(root_id, runtime_url=runtime_url), RuntimeMcpClient(runtime_url, mcp_host, rpc_timeout_s))
    return create
