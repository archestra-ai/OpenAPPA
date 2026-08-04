"""Typed adapter over the framework-owned lifecycle in ``appa_agent_python``."""

import json
from dataclasses import dataclass
from typing import Literal

import appa_agent_python
from tau2.environment.tool import Tool

from appa_taubench.knowledge import DISCOVERABLE_WRAPPER

BINDING_IDENTITY: str = appa_agent_python.BINDING_IDENTITY
AppaError = appa_agent_python.AppaError
EXECUTE_REMEDY_PLAN = "execute_remedy_plan"


class NativeProtocolError(RuntimeError):
    """The native extension returned an invalid result envelope."""


@dataclass(frozen=True)
class Blocked:
    feedback: str


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
        self._pending = False
        self._closed = False

    def check(self, tool: str, arguments: dict[str, object]) -> CheckResult:
        try:
            policy_tool, policy_arguments = self.logical_call(tool, arguments)
        except ValueError as error:
            return Blocked(f"Invalid discoverable tool call: {error}")
        response = self._decode(
            self._session.check(
                policy_tool,
                json.dumps(policy_arguments, separators=(",", ":")),
            )
        )
        match response.get("kind"):
            case "blocked" if set(response) == {"kind", "feedback"}:
                feedback = response["feedback"]
                if isinstance(feedback, str):
                    return Blocked(feedback)
            case "allowed" if set(response) == {
                "kind",
                "dispatched_tool",
                "dispatched_arguments",
            }:
                dispatched_tool = response["dispatched_tool"]
                dispatched_arguments = response["dispatched_arguments"]
                if isinstance(dispatched_tool, str) and isinstance(dispatched_arguments, dict):
                    if dispatched_tool not in self._available_tool_names:
                        raise NativeProtocolError("native check dispatched an unavailable Tau tool")
                    if policy_tool != EXECUTE_REMEDY_PLAN and (
                        dispatched_tool != policy_tool or dispatched_arguments != policy_arguments
                    ):
                        raise NativeProtocolError("native check altered an allowed Tau tool call")
                    dispatch = self._tau_dispatch(dispatched_tool, dispatched_arguments)
                    self._pending = True
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

    def report(self, content: str | None, error: bool) -> Reported:
        response = self._decode(self._session.report(content, error))
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
                self._pending = False
                return Reported(delivered_content, disposition)
        raise NativeProtocolError("native report response has an invalid result envelope")

    def new_round(self) -> None:
        self._session.new_round()

    def close(self) -> None:
        if self._closed:
            return
        if self._pending:
            self._session.abandon()
            self._pending = False
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
