"""Real v0.16.0 lifecycle adapter for the corrected proxy contract."""
from __future__ import annotations
import json
import os
import threading
from dataclasses import dataclass
from typing import Any, Mapping
from urllib.parse import urlsplit
from .appa_gate import Decision, GateError
from .appa_source_gate import SourceGate
from .appa_review_gate import ReviewGateError, RuntimeMcpClient

@dataclass(frozen=True)
class EndpointSettings:
    runtime_url: str
    mcp_host: str

    def __post_init__(self) -> None:
        parsed = urlsplit(self.runtime_url)
        if parsed.scheme != "http" or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
            raise ValueError("runtime URL must be an absolute http URL without credentials, query, or fragment")
        if not isinstance(self.mcp_host, str) or not self.mcp_host or ":" not in self.mcp_host:
            raise ValueError("MCP host must be host:port")


def _spawn_tools() -> dict[str, str]:
    raw = os.environ.get("APPA_LIFECYCLE_SPAWN_TOOL_MAP", "")
    if not raw:
        return {}
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise GateError("APPA_LIFECYCLE_SPAWN_TOOL_MAP must be JSON") from error
    if not isinstance(value, dict):
        raise GateError("APPA_LIFECYCLE_SPAWN_TOOL_MAP must map observed names to raw agent tools")
    for observed, target in value.items():
        if not isinstance(observed, str) or not isinstance(target, str) or not target.startswith("agent:"):
            raise GateError("every lifecycle spawn tool mapping must be a name to agent:<namespace>/<agent>")
    # Codex Responses qualifies the native collaboration tool as
    # agents:spawn_agent while the configured mapping names its tool leaf.
    if "spawn_agent" in value:
        value.setdefault("agents:spawn_agent", value["spawn_agent"])
        value.setdefault("multi_agent_v1:spawn_agent", value["spawn_agent"])
    value["agents:wait_agent"] = "builtin:wait_agent"
    value["multi_agent_v1:wait_agent"] = "builtin:wait_agent"
    return value


def _return_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)

def _return_floor() -> dict[str, Any]:
    raw = os.environ.get("APPA_LIFECYCLE_RETURN_FLOOR")
    if not raw:
        raise GateError("APPA_LIFECYCLE_RETURN_FLOOR is required for lifecycle spawns")
    try:
        floor = json.loads(raw)
    except json.JSONDecodeError as error:
        raise GateError("APPA_LIFECYCLE_RETURN_FLOOR must be JSON") from error
    if not isinstance(floor, dict) or not isinstance(floor.get("trust"), str):
        raise GateError("return floor requires a trust string")
    audience = floor.get("audience")
    if not isinstance(audience, list) or not audience or not all(isinstance(item, str) and item for item in audience):
        raise GateError("return floor requires a non-empty audience array")
    return floor

class LifecycleGate:
    """Maps corrected proxy calls to durable runtime events without local policy decisions."""

    def __init__(self, trajectory_id: str, endpoints: EndpointSettings, adapters: dict[str, "LifecycleGate"], lock: threading.RLock):
        self.trajectory_id = trajectory_id
        self._endpoints = endpoints
        self._adapters = adapters
        self._lock = lock
        self._gate = SourceGate(trajectory_id, runtime_url=endpoints.runtime_url)
        self._family_gate = self._gate
        self._child_id: str | None = None
        self._parent_trajectory: str | None = None
        self._spawn_tools = _spawn_tools()
        self._spawn_bindings: dict[str, str] = {}
        self._returns: dict[str, tuple[str, str, Decision]] = {}

    def session_start(self) -> Decision:
        return self._family_gate.session_start()

    def prompt(self, text: str) -> Decision:
        if self._child_id is not None:
            return self._family_gate.child_prompt(self._child_id, text)
        return self._family_gate.prompt(text)

    def before_call(self, call_id: str, name: str, args: Any) -> Decision:
        raw_name = self._spawn_tools.get(name, name)
        if self._child_id is not None:
            decision = self._family_gate.child_before_call(self._child_id, call_id, raw_name, args)
            if decision.name == "deny_call":
                decision = self._accept_fixture_ingress(call_id, raw_name, args, decision, self._child_id)
            return decision
        decision = self._family_gate.before_call(call_id, raw_name, args)
        if raw_name.startswith("agent:") and decision.name == "deny_call":
            decision = self._declare_spawn(call_id, raw_name, args, decision)
        if decision.name == "deny_call":
            decision = self._accept_fixture_ingress(call_id, raw_name, args, decision, None)
        if decision.allowed and decision.spawn_binding:
            self._spawn_bindings[call_id] = decision.spawn_binding
        return decision

    def _declare_spawn(self, call_id: str, raw_name: str, args: Any, denied: Decision) -> Decision:
        offers = denied.offers
        if len(offers) != 1 or denied.payload.get("review"):
            return denied
        offer_id = offers[0].get("offer_id") if isinstance(offers[0], Mapping) else None
        if not isinstance(offer_id, str) or not offer_id:
            return denied
        control_args = {"offer_id": offer_id, "label": _return_floor()}
        control = self._family_gate._post("tool_call", tool="appa:execute_remedy_plan", arguments=control_args)
        if control.name != "pass_control":
            return denied
        try:
            client = RuntimeMcpClient(self._endpoints.runtime_url, mcp_host=self._endpoints.mcp_host)
            client._initialize_if_needed()
            result = client._request("tools/call", {"name": "execute_remedy_plan", "arguments": control_args})
        except ReviewGateError as error:
            raise GateError("runtime did not authorize the lifecycle return floor") from error
        if result.failed:
            raise GateError("runtime refused the lifecycle return floor")
        return self._family_gate.before_call(call_id, raw_name, args)
    def _accept_fixture_ingress(
        self, call_id: str, raw_name: str, args: Any, denied: Decision, child_id: str | None
    ) -> Decision:
        if os.environ.get("APPA_LIFECYCLE_ACCEPT_FIXTURE_INGRESS") != "true":
            return denied
        if raw_name not in {"mcp__appa_fixture__read_source", "mcp__appa_fixture.read_source", "appa_fixture_read_source", "builtin:mcp__appa_fixture__read_source", "builtin:mcp__appa_fixture.read_source", "builtin:appa_fixture_read_source"} or not isinstance(args, Mapping):
            return denied
        if set(args) != {"kind"} or args.get("kind") not in {"private", "suspicious"}:
            return denied
        offers = denied.offers
        if len(offers) != 1 or denied.payload.get("review"):
            return denied
        offer_id = offers[0].get("offer_id") if isinstance(offers[0], Mapping) else None
        if not isinstance(offer_id, str) or not offer_id:
            return denied
        control_args = {"offer_id": offer_id}
        fields: dict[str, Any] = {"tool": "appa:execute_remedy_plan", "arguments": control_args}
        if child_id is not None:
            fields["child_id"] = child_id
        control = self._family_gate._post("tool_call", **fields)
        if control.name != "pass_control":
            return denied
        try:
            client = RuntimeMcpClient(self._endpoints.runtime_url, mcp_host=self._endpoints.mcp_host)
            client._initialize_if_needed()
            result = client._request("tools/call", {"name": "execute_remedy_plan", "arguments": control_args})
        except ReviewGateError as error:
            raise GateError("runtime did not authorize fixture ingress") from error
        if result.failed:
            raise GateError("runtime refused fixture ingress")
        if child_id is None:
            return self._family_gate.before_call(call_id, raw_name, args)
        return self._family_gate.child_before_call(child_id, call_id, raw_name, args)

    def after_result(self, call_id: str, *, body: Any = None, error: str | None = None) -> Decision:
        if self._child_id is not None:
            return self._family_gate.child_after_result(self._child_id, call_id, body=body, error=error)
        returned = self._returns.get(call_id)
        if returned is None:
            return self._family_gate.after_result(call_id, body=body, error=error)
        child_id, value, _ = returned
        decision = self._family_gate.spawn_result(call_id, child_id=child_id, value=value, body=body, error=error)
        if decision.name == "ack":
            del self._returns[call_id]
        return decision
    def async_spawn_ack(self, parent_call_id: str, child_trajectory_id: str, body: Any) -> Decision:
        """Record a spawn launch without claiming a child return."""
        if self._child_id is not None:
            raise GateError("only the parent adapter may acknowledge an async spawn")
        return self._family_gate.spawn_result(parent_call_id, child_id=child_trajectory_id, body=body)

    def turn_end(self) -> Decision:
        if self._child_id is not None:
            return self._family_gate.child_turn_end(self._child_id)
        return self._family_gate.turn_end()

    def child_start(
        self,
        *,
        trajectory_id: str,
        parent_trajectory_id: str,
        parent_call_id: str,
        principal_scope: str,
        inherited_checkpoint: str,
    ) -> Decision:
        if trajectory_id != self.trajectory_id:
            raise GateError("child_start trajectory does not match this adapter")
        if not principal_scope or not inherited_checkpoint:
            raise GateError("child_start requires the proxy-validated scope and checkpoint anchor")
        with self._lock:
            parent = self._adapters.get(parent_trajectory_id)
        if parent is None:
            raise GateError("child_start parent adapter is unavailable")
        binding = parent._spawn_bindings.get(parent_call_id)
        if binding is None:
            raise GateError("parent call has no runtime-issued spawn binding")
        if self._parent_trajectory is not None and self._parent_trajectory != parent_trajectory_id:
            raise GateError("child adapter is already bound to another parent")
        decision = parent._family_gate.child_start(self.trajectory_id, spawn_binding=binding)
        if decision.name != "ack":
            raise GateError("runtime did not acknowledge the child binding")
        self._family_gate = parent._family_gate
        self._child_id = self.trajectory_id
        self._parent_trajectory = parent_trajectory_id
        return decision

    def child_return(
        self,
        *,
        parent_trajectory_id: str,
        parent_call_id: str,
        child_trajectory_id: str,
        result: Any,
    ) -> Decision:
        if parent_trajectory_id != self.trajectory_id:
            raise GateError("child_return must be sent to its parent adapter")
        with self._lock:
            child = self._adapters.get(child_trajectory_id)
        if child is None or child._parent_trajectory != self.trajectory_id:
            raise GateError("child_return has no bound native child")
        value = _return_text(result)
        previous = self._returns.get(parent_call_id)
        if previous is not None:
            if previous[:2] != (child_trajectory_id, value):
                raise GateError("child return replay changed its native identity")
            return previous[2]
        decision = self._family_gate.child_end(child_trajectory_id, value)
        if decision.name == "child_return":
            replacement = decision.payload.get("value")
            if not isinstance(replacement, str):
                raise GateError("runtime returned a malformed child return derivation")
            value = replacement
            decision = self._family_gate.child_end(child_trajectory_id, value)
        if decision.name != "ack":
            raise GateError("runtime withheld the child return")
        self._returns[parent_call_id] = (child_trajectory_id, value, decision)
        return decision


def factory(runtime_url: str, mcp_host: str):
    """Bind runtime endpoints once and isolate adapters to this proxy instance."""
    endpoints = EndpointSettings(runtime_url, mcp_host)
    adapters: dict[str, LifecycleGate] = {}
    lock = threading.RLock()

    def create(trajectory_id: str) -> LifecycleGate:
        if not isinstance(trajectory_id, str) or not trajectory_id:
            raise ValueError("trajectory_id must be a non-empty string")
        with lock:
            adapter = adapters.get(trajectory_id)
            if adapter is None:
                adapter = LifecycleGate(trajectory_id, endpoints, adapters, lock)
                adapters[trajectory_id] = adapter
            return adapter

    return create
