"""Source-runtime hook and detached-checkpoint client."""
from __future__ import annotations
import json
from pathlib import Path
from typing import Any, Mapping
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
from .appa_gate import ADAPTER, PROTOCOL, Decision, Gate, GateError, _OpenCall
DEFAULT_SOURCE_RUNTIME_URL = "http://127.0.0.1:8787"
class SourceGate(Gate):
    def __init__(self, root_id: str, policy: str | None = None, *, runtime_url: str = DEFAULT_SOURCE_RUNTIME_URL, timeout: float = 10.0, trace_path: str | Path | None = None) -> None:
        super().__init__(root_id, policy, runtime_url=runtime_url, timeout=timeout, trace_path=trace_path)
    def before_call(self, call_id: str, name: str, args: Any) -> Decision:
        self._require_started()
        self._check_new_call_id(call_id, child_id=None)
        raw_name = self._raw_name(name)
        arguments = self._canonical_arguments(args)
        decision = self._post("tool_call", tool=raw_name, arguments=json.loads(arguments))
        if decision.allowed:
            self._open_calls[(None, call_id)] = _OpenCall(raw_name=raw_name, arguments=arguments)
        return decision
    def child_before_call(self, child_id: str, call_id: str, name: str, args: Any) -> Decision:
        self._require_child(child_id)
        self._check_new_call_id(call_id, child_id=child_id)
        raw_name = self._raw_name(name)
        arguments = self._canonical_arguments(args)
        decision = self._post("tool_call", child_id=child_id, tool=raw_name, arguments=json.loads(arguments))
        if decision.allowed:
            self._open_calls[(child_id, call_id)] = _OpenCall(raw_name=raw_name, arguments=arguments)
        return decision
    def begin_remedy(self, offer_id: str) -> Decision:
        self._require_started()
        if not isinstance(offer_id, str) or not offer_id:
            raise ValueError("offer_id must be a non-empty string")
        return self._post("tool_call", tool="appa:execute_remedy_plan", arguments={"offer_id": offer_id})
    def checkpoint_create(self) -> Mapping[str, Any]:
        return self._checkpoint({"operation": "create", "root_id": self.root_id}, self.root_id)
    def checkpoint_fork(self, checkpoint_id: str, target_root_id: str) -> Mapping[str, Any]:
        if not isinstance(checkpoint_id, str) or not checkpoint_id or not isinstance(target_root_id, str) or not target_root_id:
            raise ValueError("checkpoint and target root ids must be non-empty strings")
        return self._checkpoint({"operation": "fork", "checkpoint_id": checkpoint_id, "root_id": target_root_id}, target_root_id)
    def _checkpoint(self, fields: Mapping[str, Any], expect_root: str) -> Mapping[str, Any]:
        body = json.dumps({"protocol": PROTOCOL, "adapter": ADAPTER, **fields}, separators=(",", ":"), ensure_ascii=True).encode("utf-8")
        request = Request(f"{self.runtime_url}/checkpoint", data=body, headers={"Content-Type": "application/json", "Accept": "application/json"}, method="POST")
        try:
            with urlopen(request, timeout=self.timeout) as response:
                status, raw = response.status, response.read()
        except HTTPError as error:
            status, raw = error.code, error.read()
        except URLError as error:
            raise GateError(f"runtime unavailable for checkpoint: {error.reason}") from error
        try:
            reply = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise GateError(f"runtime returned non-JSON HTTP {status} for checkpoint") from error
        if status // 100 != 2 or not isinstance(reply, dict):
            raise GateError(f"runtime refused checkpoint HTTP {status}: {reply}")
        if reply.get("root_id") == expect_root:
            return reply
        scope = reply.get("source_scope")
        if isinstance(scope, dict) and scope.get("adapter") == ADAPTER and scope.get("root_id") == expect_root and isinstance(reply.get("checkpoint_id"), str):
            return reply
