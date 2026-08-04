"""Typed adapter over the framework-owned lifecycle in ``appa_agent_python``."""

import json
from dataclasses import dataclass
from typing import Literal

import appa_agent_python
from tau2.environment.tool import Tool

BINDING_IDENTITY: str = appa_agent_python.BINDING_IDENTITY
AppaError = appa_agent_python.AppaError


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

    def __init__(self, policy: str, tools: list[Tool], user_prompt: str) -> None:
        schemas = [tool.openai_schema for tool in tools]
        self._session = appa_agent_python.Session(
            policy,
            json.dumps(schemas, separators=(",", ":")),
            user_prompt,
        )
        self._pending = False
        self._closed = False

    def check(self, tool: str, arguments: dict[str, object]) -> CheckResult:
        response = self._decode(self._session.check(tool, json.dumps(arguments, separators=(",", ":"))))
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
                    self._pending = True
                    return Allowed(dispatched_tool, dispatched_arguments)
        raise NativeProtocolError("native check response has an invalid result envelope")

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

    @staticmethod
    def _decode(response_json: str) -> dict:
        try:
            response = json.loads(response_json)
        except json.JSONDecodeError as error:
            raise NativeProtocolError("native response is not valid JSON") from error
        if not isinstance(response, dict):
            raise NativeProtocolError("native response is not an object")
        return response
