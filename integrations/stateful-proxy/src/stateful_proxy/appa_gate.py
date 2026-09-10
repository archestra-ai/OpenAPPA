"""Stateful OpenAPPA /hook client for the stateful-proxy fixture.

The deployed runtime is configured with the kagent adapter. This client therefore
uses its protocol-compatible ``builtin:<name>`` raw spellings. It is not a
kagent client and does not assert kagent identity beyond that adapter transport.
Policy selection belongs to the runtime deployment; ``policy`` is trace metadata.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final, Mapping
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

DEFAULT_RUNTIME_URL: Final = "http://127.0.0.1:18788"
PROTOCOL: Final = 1
ADAPTER: Final = "kagent"
_MISSING: Final = object()
_SETTLED_RESULT_DECISIONS: Final = frozenset({"ack", "deliver_value", "replace_output", "block"})


class GateError(RuntimeError):
    """The runtime did not supply a usable decision; callers must fail closed."""


@dataclass(frozen=True)
class Decision:
    """One runtime decision envelope returned by ``POST /hook``."""

    name: str
    payload: Mapping[str, Any]

    @property
    def allowed(self) -> bool:
        """Whether the harness may execute the proposed ordinary tool call."""
        return self.name == "allow_call"

    @property
    def denied(self) -> bool:
        """Whether the harness must not execute the proposed ordinary tool call."""
        return self.name in {"deny_call", "block", "refuse"}

    @property
    def offers(self) -> tuple[Mapping[str, Any], ...]:
        """Engine-generated remedy offers, if this decision is ``deny_call``."""
        offers = self.payload.get("offers")
        return tuple(offers) if isinstance(offers, list) else ()

    @property
    def spawn_binding(self) -> str | None:
        """The opaque fork checkpoint released for an admitted spawn."""
        binding = self.payload.get("spawn_binding")
        return binding if isinstance(binding, str) and binding else None



@dataclass(frozen=True)
class _OpenCall:
    raw_name: str
    arguments: str


class Gate:
    """Maintain one root trajectory while synchronously gating a proxy's tools.

    Typical usage::

        gate = Gate("client-run-42")
        gate.session_start()
        gate.prompt("read the public fixture")
        decision = gate.before_call("read-1", "read_fixture", {"path": "public.txt"})
        if decision.allowed:
            gate.after_result("read-1", body={"text": "fixture contents"})
        gate.turn_end()

    ``before_call`` records a call only after an ``allow_call`` decision. The
    matching ``after_result`` always reuses the exact raw tool spelling and the
    single canonical JSON argument encoding used for that allowed call.
    """

    def __init__(
        self,
        root_id: str,
        policy: str | None = None,
        *,
        runtime_url: str = DEFAULT_RUNTIME_URL,
        timeout: float = 10.0,
        trace_path: str | Path | None = None,
    ) -> None:
        if not isinstance(root_id, str) or not root_id:
            raise ValueError("root_id must be a non-empty string")
        if timeout <= 0:
            raise ValueError("timeout must be positive")
        self.root_id = root_id
        # Policy is deployment-owned TOML, never an untrusted client override.
        self.policy = policy
        self.runtime_url = runtime_url.rstrip("/")
        self.timeout = timeout
        self.trace_path = Path(trace_path) if trace_path is not None else None
        self._started = False
        self._open_calls: dict[tuple[str | None, str], _OpenCall] = {}
        self._children: dict[str, str | None] = {}

    def session_start(self) -> Decision:
        """Open the root trajectory. The runtime must return ``ack``."""
        if self._started:
            raise GateError("session_start was already sent for this Gate")
        decision = self._post("session_start")
        self._require(decision, {"ack"}, "session_start")
        self._started = True
        return decision

    def prompt(self, text: str) -> Decision:
        """Record a model-input boundary. Prompt events must return ``ack``."""
        self._require_started()
        if not isinstance(text, str):
            raise TypeError("text must be a string")
        decision = self._post("prompt", text=text)
        self._require(decision, {"ack"}, "prompt")
        return decision

    def child_start(
        self,
        child_id: str,
        *,
        spawn_binding: str | None = None,
        inventory: Mapping[str, Any] | None = None,
    ) -> Decision:
        """Bind a child to one opaque runtime-issued fork checkpoint."""
        self._require_started()
        self._check_child_id(child_id)
        if spawn_binding is not None and (not isinstance(spawn_binding, str) or not spawn_binding):
            raise ValueError("spawn_binding must be a non-empty string or None")
        if inventory is not None and not isinstance(inventory, Mapping):
            raise TypeError("inventory must be a mapping or None")
        previous = self._children.get(child_id, _MISSING)
        if previous is not _MISSING and previous != spawn_binding:
            raise GateError(f"child {child_id!r} was already started with another spawn binding")
        fields: dict[str, Any] = {"child_id": child_id}
        if spawn_binding is not None:
            fields["spawn_binding"] = spawn_binding
        if inventory is not None:
            fields["inventory"] = dict(inventory)
        decision = self._post("child_start", **fields)
        self._require(decision, {"ack", "context"}, "child_start")
        self._children[child_id] = spawn_binding
        return decision

    def child_prompt(self, child_id: str, text: str) -> Decision:
        """Record prompt context for one started child."""
        self._require_child(child_id)
        if not isinstance(text, str):
            raise TypeError("text must be a string")
        decision = self._post("prompt", child_id=child_id, text=text)
        self._require(decision, {"ack"}, "child_prompt")
        return decision

    def child_before_call(self, child_id: str, call_id: str, name: str, args: Any) -> Decision:
        """Gate a tool call under the child fork's inherited state."""
        self._require_child(child_id)
        self._check_new_call_id(call_id, child_id=child_id)
        raw_name = self._raw_name(name)
        arguments = self._canonical_arguments(args)
        fields: dict[str, Any] = {"child_id": child_id, "tool": raw_name, "arguments": json.loads(arguments), "spawn": False}
        decision = self._post("tool_call", **fields)
        if decision.allowed:
            self._open_calls[(child_id, call_id)] = _OpenCall(raw_name=raw_name, arguments=arguments)
        return decision

    def child_after_result(
        self,
        child_id: str,
        call_id: str,
        *,
        body: Any = _MISSING,
        error: str | None = None,
        indeterminate: bool = False,
    ) -> Decision:
        """Report a child tool result before it enters child context."""
        self._require_child(child_id)
        key = (child_id, call_id)
        call = self._open_calls.get(key)
        if call is None:
            raise GateError(f"no allowed call is open for {call_id!r} on this child")
        outcome = self._outcome(body=body, error=error, indeterminate=indeterminate)
        decision = self._post("tool_result", child_id=child_id, tool=call.raw_name, arguments=json.loads(call.arguments), outcome=outcome)
        if decision.name in _SETTLED_RESULT_DECISIONS:
            del self._open_calls[key]
        return decision

    def child_turn_end(self, child_id: str) -> Decision:
        """Close one child turn without closing its root trajectory."""
        self._require_child(child_id)
        decision = self._post("turn_end", child_id=child_id)
        self._require(decision, {"ack"}, "child_turn_end")
        self._clear_actor_calls(child_id)
        return decision
    def child_end(self, child_id: str, value: str | None = None) -> Decision:
        """Check a child answer at its return boundary before it reaches the parent."""
        self._require_child(child_id)
        if value is not None and not isinstance(value, str):
            raise TypeError("value must be a string or None")
        fields: dict[str, Any] = {"child_id": child_id}
        if value is not None:
            fields["value"] = value
        return self._post("child_end", **fields)

    def spawn_result(
        self,
        call_id: str,
        *,
        child_id: str | None = None,
        value: str | None = None,
        body: Any = _MISSING,
        error: str | None = None,
        indeterminate: bool = False,
        actor_child_id: str | None = None,
    ) -> Decision:
        """Settle a derived spawn and replay only a value that crossed at child_end."""
        self._require_started()
        if actor_child_id is not None:
            self._require_child(actor_child_id)
        if child_id is not None:
            self._check_child_id(child_id)
        if value is not None and not isinstance(value, str):
            raise TypeError("value must be a string or None")
        key = (actor_child_id, call_id)
        call = self._open_calls.get(key)
        if call is None:
            raise GateError(f"no allowed spawn is open for {call_id!r} on this actor")
        fields: dict[str, Any] = {
            "tool": call.raw_name,
            "arguments": json.loads(call.arguments),
            "outcome": self._outcome(body=body, error=error, indeterminate=indeterminate),
        }
        if actor_child_id is not None:
            fields["child_id"] = actor_child_id
        if child_id is not None:
            fields["spawned_id"] = child_id
        if value is not None:
            fields["value"] = value
        decision = self._post("spawn_result", **fields)
        if decision.name in _SETTLED_RESULT_DECISIONS:
            del self._open_calls[key]
        return decision

    def resume_call(self, call_id: str, name: str, args: Any, *, child_id: str | None = None) -> None:
        """Restore exact local call correlation after proxy restart or compaction."""
        self._require_started()
        if child_id is not None:
            self._require_child(child_id)
        self._check_new_call_id(call_id, child_id=child_id)
        self._open_calls[(child_id, call_id)] = _OpenCall(
            raw_name=self._raw_name(name), arguments=self._canonical_arguments(args)
        )

    def before_call(self, call_id: str, name: str, args: Any) -> Decision:

        """Gate one proposed tool call before the proxy executes it.
        ``name`` may be a client tool name such as ``read_fixture`` or a raw
        kagent-adapter spelling such as ``builtin:read_fixture``. An allowed
        call returns ``allow_call``. Any other decision must prevent execution.
        """
        self._require_started()
        self._check_new_call_id(call_id, child_id=None)
        raw_name = self._raw_name(name)
        arguments = self._canonical_arguments(args)
        fields: dict[str, Any] = {"tool": raw_name, "arguments": json.loads(arguments), "spawn": False}
        decision = self._post("tool_call", **fields)
        if decision.allowed:
            self._open_calls[(None, call_id)] = _OpenCall(raw_name=raw_name, arguments=arguments)
        return decision

    def after_result(
        self,
        call_id: str,
        *,
        body: Any = _MISSING,
        error: str | None = None,
        indeterminate: bool = False,
    ) -> Decision:
        """Report an allowed call's outcome before exposing it to the model.

        Supply exactly one outcome form: ``body`` (including ``None`` for JSON
        null), ``error`` for a failed tool, ``indeterminate=True``, or no value
        for ``success_without_body``. The result decision controls delivery.
        """
        self._require_started()
        key = (None, call_id)
        call = self._open_calls.get(key)
        if call is None:
            raise GateError(f"no allowed call is open for {call_id!r}")
        outcome = self._outcome(body=body, error=error, indeterminate=indeterminate)
        decision = self._post("tool_result", tool=call.raw_name, arguments=json.loads(call.arguments), outcome=outcome)
        if decision.name in _SETTLED_RESULT_DECISIONS:
            del self._open_calls[key]
        return decision

    def begin_remedy(self, offer_id: str) -> Decision:
        """Vouch one engine-issued offer before an MCP ``execute_remedy_plan`` call.

        A successful response is ``pass_control``. This method never supplies a
        human ruling. The caller must use the runtime's MCP review path for an
        authority-backed offer rather than fabricate an approval.
        """
        self._require_started()
        if not isinstance(offer_id, str) or not offer_id:
            raise ValueError("offer_id must be a non-empty string")
        decision = self._post(
            "tool_call",
            tool="execute_remedy_plan",
            spawn=False,
            arguments={"offer_id": offer_id},
        )
        self._require(decision, {"allow_call", "pass_control", "deny_call", "refuse"}, "begin_remedy")
        return decision

    def turn_end(self) -> Decision:
        """End the current turn and settle any unreported dispatches as unknown."""
        self._require_started()
        decision = self._post("turn_end")
        self._require(decision, {"ack"}, "turn_end")
        self._clear_actor_calls(None)
        return decision

    def _post(self, event: str, **fields: Any) -> Decision:
        envelope = {"protocol": PROTOCOL, "adapter": ADAPTER, "event": event, "root_id": self.root_id, **fields}
        self._trace("request", envelope)
        body = json.dumps(envelope, separators=(",", ":"), ensure_ascii=True).encode("utf-8")
        request = Request(
            f"{self.runtime_url}/hook",
            data=body,
            headers={"Content-Type": "application/json", "Accept": "application/json"},
            method="POST",
        )
        try:
            with urlopen(request, timeout=self.timeout) as response:
                status = response.status
                raw = response.read()
        except HTTPError as error:
            status = error.code
            raw = error.read()
        except URLError as error:
            raise GateError(f"runtime unavailable for {event}: {error.reason}") from error
        except OSError as error:
            raise GateError(f"runtime I/O failed for {event}: {error}") from error

        try:
            payload = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise GateError(f"runtime returned non-JSON HTTP {status} for {event}") from error
        self._trace("response", {"status": status, "payload": payload})
        # The live runtime's HTTP handler returns a decision object, not a
        # complete request-shaped envelope. Protocol 1 is mandatory on requests;
        # a response is usable when it contains one named decision.
        if not isinstance(payload, dict) or not isinstance(payload.get("decision"), str):
            raise GateError(f"runtime returned an invalid decision for {event}: HTTP {status}")
        return Decision(name=payload["decision"], payload=payload)

    @staticmethod
    def _raw_name(name: str) -> str:
        if not isinstance(name, str) or not name:
            raise ValueError("name must be a non-empty string")
        return name if ":" in name else f"builtin:{name}"

    @staticmethod
    def _canonical_arguments(args: Any) -> str:
        try:
            return json.dumps(args, separators=(",", ":"), ensure_ascii=True, allow_nan=False)
        except (TypeError, ValueError) as error:
            raise TypeError("args must be JSON-serializable without NaN values") from error

    @staticmethod
    def _outcome(*, body: Any, error: str | None, indeterminate: bool) -> Mapping[str, Any]:
        selected = int(body is not _MISSING) + int(error is not None) + int(indeterminate)
        if selected > 1:
            raise ValueError("choose only one of body, error, or indeterminate")
        if indeterminate:
            return {"status": "indeterminate"}
        if error is not None:
            if not isinstance(error, str):
                raise TypeError("error must be a string")
            return {"status": "failure", "message": error}
        if body is _MISSING:
            return {"status": "success_without_body"}
        Gate._canonical_arguments(body)
        return {"status": "success", "body": body}

    def _check_new_call_id(self, call_id: str, *, child_id: str | None) -> None:
        if not isinstance(call_id, str) or not call_id:
            raise ValueError("call_id must be a non-empty string")
        if (child_id, call_id) in self._open_calls:
            raise GateError(f"call_id {call_id!r} is already open on this actor")

    @staticmethod
    def _check_child_id(child_id: str) -> None:
        if not isinstance(child_id, str) or not child_id:
            raise ValueError("child_id must be a non-empty string")

    def _require_child(self, child_id: str) -> None:
        self._require_started()
        self._check_child_id(child_id)
        if child_id not in self._children:
            raise GateError(f"child_start must bind {child_id!r} before child events")

    def _clear_actor_calls(self, child_id: str | None) -> None:
        for key in [key for key in self._open_calls if key[0] == child_id]:
            del self._open_calls[key]

    def _require_started(self) -> None:
        if not self._started:
            raise GateError("call session_start before sending trajectory events")

    @staticmethod
    def _require(decision: Decision, allowed: set[str], event: str) -> None:
        if decision.name not in allowed:
            raise GateError(f"runtime returned {decision.name!r} for {event}, expected one of {sorted(allowed)!r}")

    def _trace(self, direction: str, value: Mapping[str, Any]) -> None:
        if self.trace_path is None:
            return
        self.trace_path.parent.mkdir(parents=True, exist_ok=True)
        record = {"direction": direction, **value}
        with self.trace_path.open("a", encoding="utf-8") as trace:
            trace.write(json.dumps(record, separators=(",", ":"), ensure_ascii=True) + "\n")
