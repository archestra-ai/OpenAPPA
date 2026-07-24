"""Typed adapter over the synchronous ``appa_agent_python`` extension."""

import json
from dataclasses import dataclass
from typing import Literal, TypeAlias

import appa_agent_python

BINDING_IDENTITY: str = appa_agent_python.BINDING_IDENTITY
AppaError = appa_agent_python.AppaError


class NativeProtocolError(RuntimeError):
    """The native extension returned an invalid result envelope."""


@dataclass(frozen=True)
class Blocked:
    feedback: str


@dataclass(frozen=True)
class Delivered:
    content: str
    dispatched_tool: str
    dispatched_arguments: dict[str, object]
    disposition: Literal["admitted", "sealed"]


DispatchResult: TypeAlias = Blocked | Delivered


class NativeSession:
    """One native CallSession whose dispatch method owns each complete transaction."""

    def __init__(self, policy: str, tools: list[str], user_prompt: str, bridge_url: str) -> None:
        self._session = appa_agent_python.Session(
            policy,
            json.dumps(tools, separators=(",", ":")),
            user_prompt,
            bridge_url,
        )
        self._closed = False

    def dispatch(self, tool: str, arguments: dict[str, object]) -> DispatchResult:
        response_json = self._session.dispatch(tool, json.dumps(arguments, separators=(",", ":")))
        try:
            response = json.loads(response_json)
        except json.JSONDecodeError as error:
            raise NativeProtocolError("native response is not valid JSON") from error
        if not isinstance(response, dict):
            raise NativeProtocolError("native response is not an object")
        match response.get("kind"):
            case "blocked" if set(response) == {"kind", "feedback"}:
                feedback = response["feedback"]
                if isinstance(feedback, str):
                    return Blocked(feedback)
            case "delivered" if set(response) == {
                "kind",
                "content",
                "dispatched_tool",
                "dispatched_arguments",
                "disposition",
            }:
                content = response["content"]
                dispatched_tool = response["dispatched_tool"]
                dispatched_arguments = response["dispatched_arguments"]
                disposition = response["disposition"]
                if (
                    isinstance(content, str)
                    and isinstance(dispatched_tool, str)
                    and isinstance(dispatched_arguments, dict)
                    and disposition in {"admitted", "sealed"}
                ):
                    return Delivered(content, dispatched_tool, dispatched_arguments, disposition)
        raise NativeProtocolError("native response has an invalid result envelope")

    def new_round(self) -> None:
        """Signal a new model completion. Informed acceptance requires it: an acceptance-carrying
        remedy executes only in a round after the one that surfaced its offer."""
        self._session.new_round()

    def close(self) -> None:
        if self._closed:
            return
        self._session.close()
        self._closed = True

    def __enter__(self) -> "NativeSession":
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> None:
        self.close()
