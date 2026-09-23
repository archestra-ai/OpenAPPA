"""Typed adapter over the framework-owned lifecycle in ``appa_agent_python``."""

import json
from dataclasses import dataclass
from typing import Literal

import appa_agent_python
from tau2.environment.tool import Tool

from appa_taubench.knowledge import DISCOVERABLE_WRAPPER

BINDING_IDENTITY: str = appa_agent_python.BINDING_IDENTITY
AppaError = appa_agent_python.AppaError


class NativeProtocolError(RuntimeError):
    """The native extension returned an invalid result envelope."""


@dataclass(frozen=True)
class Blocked:
    feedback: str
    recoverable: bool = False


@dataclass(frozen=True)
class Allowed:
    dispatched_tool: str
    dispatched_arguments: dict[str, object]


@dataclass(frozen=True)
class Reported:
    content: str
    disposition: Literal["admitted", "sealed"]


type CheckResult = Blocked | Allowed


class FrameworkSession:
    """One CallSession whose allowed tools execute in TauBench."""

    def __init__(
        self,
        policy: str,
        tools: list[Tool],
        user_prompt: str,
        logical_tools: list[Tool] | None = None,
    ) -> None:
        logical_tools = logical_tools or []
        model_names = {tool.name for tool in tools}
        logical_names = {tool.name for tool in logical_tools}
        overlap = model_names & logical_names
        if overlap:
            raise ValueError(f"logical Tau tools collide with model tools: {sorted(overlap)}")
        schemas = [tool.openai_schema for tool in [*tools, *logical_tools]]
        self._session = appa_agent_python.Session(
            policy,
            json.dumps(schemas, separators=(",", ":")),
            user_prompt,
        )
        self._logical_tool_names = logical_names
        self._available_tool_names = model_names | logical_names
        self._pending: set[str | None] = set()
        self._closed = False

    def check(self, tool: str, arguments: dict[str, object], call_id: str | None = None) -> CheckResult:
        try:
            policy_tool, policy_arguments = self.logical_call(tool, arguments)
        except ValueError as error:
            return Blocked(f"Invalid discoverable tool call: {error}")
        encoded_arguments = json.dumps(policy_arguments, separators=(",", ":"))
        response_json = (
            self._session.check(policy_tool, encoded_arguments)
            if call_id is None
            else self._session.check(policy_tool, encoded_arguments, call_id=call_id)
        )
        response = self._decode(response_json)
        match response.get("kind"):
            case "blocked" if set(response) == {"kind", "feedback"}:
                feedback = response["feedback"]
                if isinstance(feedback, str):
                    return Blocked(feedback, self._has_next_step(feedback))
            case "control" if set(response) == {"kind", "reply"}:
                # The engine's own control tool answered. This contract mints no
                # offers, so the reply is a refusal's next step rather than an
                # authorization; hand it back as feedback the model can act on.
                reply = response["reply"]
                if isinstance(reply, str):
                    return Blocked(reply, recoverable=True)
            case "allowed" if set(response) <= {
                "kind",
                "dispatched_tool",
                "dispatched_arguments",
                "spawn_binding",
            }:
                dispatched_tool = response["dispatched_tool"]
                dispatched_arguments = response["dispatched_arguments"]
                if isinstance(dispatched_tool, str) and isinstance(dispatched_arguments, dict):
                    if dispatched_tool not in self._available_tool_names:
                        raise NativeProtocolError("native check dispatched an unavailable Tau tool")
                    if dispatched_tool != policy_tool or dispatched_arguments != policy_arguments:
                        raise NativeProtocolError("native check altered an allowed Tau tool call")
                    dispatch = self._tau_dispatch(dispatched_tool, dispatched_arguments)
                    self._pending.add(call_id)
                    return dispatch
        raise NativeProtocolError("native check response has an invalid result envelope")

    def logical_call(
        self,
        tool: str,
        arguments: dict[str, object],
    ) -> tuple[str, dict[str, object]]:
        """Resolve Tau's discoverable-tool wrapper to the logical APPA call."""
        if tool != DISCOVERABLE_WRAPPER:
            return tool, arguments
        extra_arguments = set(arguments) - {"agent_tool_name", "arguments"}
        if extra_arguments:
            raise ValueError(f"unexpected wrapper arguments: {sorted(extra_arguments)}")
        logical_tool = arguments.get("agent_tool_name")
        if not isinstance(logical_tool, str) or logical_tool not in self._logical_tool_names:
            raise ValueError("agent_tool_name does not name a registered discoverable tool")
        encoded_arguments = arguments.get("arguments", "{}")
        if not isinstance(encoded_arguments, str):
            raise ValueError("arguments must be a JSON string")
        try:
            logical_arguments = json.loads(encoded_arguments)
        except json.JSONDecodeError as error:
            raise ValueError("arguments is not valid JSON") from error
        if not isinstance(logical_arguments, dict):
            raise ValueError("arguments must encode a JSON object")
        return logical_tool, logical_arguments

    def report(self, content: str | None, error: bool, call_id: str | None = None) -> Reported:
        response_json = (
            self._session.report(content, error)
            if call_id is None
            else self._session.report(content, error, call_id=call_id)
        )
        response = self._decode(response_json)
        if response.get("kind") == "delivered" and set(response) == {
            "kind",
            "content",
            "dispatched_tool",
            "dispatched_arguments",
            "disposition",
        }:
            delivered_content = response["content"]
            disposition = response["disposition"]
            if isinstance(delivered_content, str) and disposition in {"admitted", "sealed"}:
                self._pending.remove(call_id)
                return Reported(delivered_content, disposition)
        raise NativeProtocolError("native report response has an invalid result envelope")

    def abandon(self, call_id: str | None = None) -> None:
        if call_id is None:
            self._session.abandon()
        else:
            self._session.abandon(call_id=call_id)
        self._pending.remove(call_id)

    def close(self) -> None:
        if self._closed:
            return
        for call_id in tuple(self._pending):
            self.abandon(call_id)
        self._session.close()
        self._closed = True

    def _tau_dispatch(self, tool: str, arguments: dict[str, object]) -> Allowed:
        if tool not in self._logical_tool_names:
            return Allowed(tool, arguments)
        return Allowed(
            DISCOVERABLE_WRAPPER,
            {
                "agent_tool_name": tool,
                "arguments": json.dumps(arguments, separators=(",", ":")),
            },
        )

    @staticmethod
    def _decode(response_json: str) -> dict:
        try:
            response = json.loads(response_json)
        except json.JSONDecodeError as error:
            raise NativeProtocolError("native response is not valid JSON") from error
        if not isinstance(response, dict):
            raise NativeProtocolError("native response is not an object")
        return response

    @staticmethod
    def _has_next_step(feedback: str) -> bool:
        """Return whether block feedback names a next step the model can take.

        The engine renders remedies under a ``Continue:`` heading: redispatch
        advice ("Run log_verification first; it clears: ..."), and, where the
        policy mints them, executable offers that quote an ``offer_id``. A
        requirement on a prior effect — this contract's only requirement — has
        no authority to waive it, so its remedy is redispatch advice.
        """
        return "Continue:" in feedback.splitlines() or "offer_id:" in feedback
