"""Typed adapter over the framework-owned ``appa_agent_python`` lifecycle."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Literal

import appa_agent_python
from inspect_ai.tool import Tool, ToolDef

BINDING_IDENTITY: str = appa_agent_python.BINDING_IDENTITY
EXECUTE_REMEDY_PLAN = "execute_remedy_plan"
# The typed refusal a blocked or sealed answer states, null where it states none.
REFUSAL_FIELDS = frozenset({"deciding", "outcomes", "retry"})


def _unsettled(response: dict[str, object]) -> str | None:
    """The safe content of a report that settled nothing: a fact its result needs was not
    obtained. This harness does not retry it, so the caller abandons the call and the model
    reads that content in place of the result."""
    content = response.get("content")
    if (
        response.get("kind") == "unsettled"
        and set(response) == {"kind", "content", "retry"}
        and isinstance(content, str)
    ):
        return content
    return None


class NativeProtocolError(RuntimeError):
    """The native extension returned an invalid response envelope."""


@dataclass(frozen=True)
class Blocked:
    feedback: str
    recoverable: bool = False
    offer_ids: tuple[str, ...] = ()


@dataclass(frozen=True)
class Refused:
    """The runtime refused to judge the call and failed closed: an annotation was
    missing, inadmissible under its annotator's mandate, or malformed. This is an
    operational refusal, never policy feedback — the call was not judged."""

    detail: str


@dataclass(frozen=True)
class Control:
    reply: str


@dataclass(frozen=True)
class Allowed:
    dispatched_tool: str
    dispatched_arguments: dict[str, object]


@dataclass(frozen=True)
class Reported:
    content: str
    disposition: Literal["admitted", "sealed"]


@dataclass(frozen=True)
class Spawned:
    child_id: str
    dispatched_tool: str
    dispatched_arguments: dict[str, object]


@dataclass(frozen=True)
class Returned:
    value: str | None
    disposition: Literal["crossed", "substituted"]


type CheckResult = Blocked | Control | Allowed | Refused
type SpawnResult = Blocked | Control | Spawned
type FinishResult = Blocked | Returned


def wire_tool_schema(tool: Tool) -> dict[str, object]:
    """Render one Inspect tool as the OpenAI-compatible schema the SDK binds."""
    definition = ToolDef(tool)
    return {
        "type": "function",
        "function": {
            "name": definition.name,
            "description": definition.description,
            "parameters": definition.parameters.model_dump(exclude_none=True),
        },
    }


def _blocked(response: dict[str, object]) -> Blocked | None:
    """The blocked envelope: the policy feedback the engine surfaced and the ids
    of the offers it quotes. A held child return's envelope names no offer ids."""
    if set(response) - REFUSAL_FIELDS not in ({"kind", "feedback"}, {"kind", "feedback", "offer_ids"}):
        return None
    feedback = response["feedback"]
    offer_ids = response.get("offer_ids", [])
    if not isinstance(feedback, str) or not isinstance(offer_ids, list):
        return None
    if not all(isinstance(offer, str) for offer in offer_ids):
        return None
    return Blocked(feedback, bool(offer_ids), tuple(offer_ids))


class NativeSession:
    """One native CallSession whose allowed calls execute in Inspect."""

    def __init__(
        self,
        policy: str,
        tools: list[Tool],
        user_prompt: str,
        externals_toml: str | None = None,
        spawn_tool: str | None = None,
    ) -> None:
        schemas = [wire_tool_schema(tool) for tool in tools]
        self._available_tools = {schema["function"]["name"] for schema in schemas}  # type: ignore[index]
        self._session = appa_agent_python.Session(
            policy,
            json.dumps(schemas, separators=(",", ":")),
            user_prompt,
            externals_toml=externals_toml,
            spawn_tool=spawn_tool,
        )
        self._pending = False
        self._closed = False
        self._spawn_tool = spawn_tool

    def spawn_child(
        self,
        child_id: str,
        return_schema: dict[str, object] | None = None,
        arguments: dict[str, object] | None = None,
    ) -> tuple[SpawnResult, NativeChildSession | None]:
        resp_json, child = self._session.spawn_child(
            child_id,
            return_schema=return_schema,
            arguments=arguments,
        )
        response = self._decode(resp_json)
        match response.get("kind"):
            case "blocked" if child is None:
                blocked = _blocked(response)
                if blocked is not None:
                    return blocked, None
            case "control" if set(response) == {"kind", "reply", "retry"}:
                reply = response["reply"]
                if isinstance(reply, str) and child is None:
                    return Control(reply), None
            case "opened" if set(response) == {
                "kind",
                "child_id",
                "dispatched_tool",
                "dispatched_arguments",
            }:
                opened_id = response["child_id"]
                dispatched_tool = response["dispatched_tool"]
                dispatched_arguments = response["dispatched_arguments"]
                expected_arguments = dict(arguments or {})
                if (
                    child is not None
                    and opened_id == child_id
                    and dispatched_tool == self._spawn_tool
                    and isinstance(dispatched_arguments, dict)
                    and dispatched_arguments == expected_arguments
                ):
                    spawned = Spawned(opened_id, dispatched_tool, dispatched_arguments)
                    return spawned, NativeChildSession(child, self._available_tools)
        raise NativeProtocolError("native spawn response has an invalid envelope")

    def check(self, tool: str, arguments: dict[str, object]) -> CheckResult:
        if self._pending:
            raise NativeProtocolError("a native call is already pending")
        try:
            raw = self._session.check(
                tool,
                json.dumps(arguments, separators=(",", ":"), ensure_ascii=False),
            )
        except appa_agent_python.AppaError as error:
            return Refused(str(error))
        response = self._decode(raw)
        match response.get("kind"):
            case "blocked":
                blocked = _blocked(response)
                if blocked is not None:
                    return blocked
            case "control" if set(response) == {"kind", "reply", "retry"}:
                reply = response["reply"]
                if isinstance(reply, str):
                    return Control(reply)
            case "allowed" if set(response) <= {
                "kind",
                "dispatched_tool",
                "dispatched_arguments",
                "spawn_binding",
            }:
                dispatched_tool = response["dispatched_tool"]
                dispatched_arguments = response["dispatched_arguments"]
                if isinstance(dispatched_tool, str) and isinstance(dispatched_arguments, dict):
                    if dispatched_tool not in self._available_tools:
                        raise NativeProtocolError("native check dispatched an unavailable Inspect tool")
                    if tool != EXECUTE_REMEDY_PLAN and (dispatched_tool != tool or dispatched_arguments != arguments):
                        raise NativeProtocolError("native check altered an ordinary Inspect call")
                    self._pending = True
                    return Allowed(dispatched_tool, dispatched_arguments)
        raise NativeProtocolError("native check response has an invalid envelope")

    def report(self, content: str | None, error: bool) -> Reported:
        if not self._pending:
            raise NativeProtocolError("no native call is pending")
        # A refused report leaves the call pending, to report again or abandon.
        response = self._decode(self._session.report(content, error))
        if (withheld := _unsettled(response)) is not None:
            self.abandon()
            return Reported(withheld, "sealed")
        self._pending = False
        if response.get("kind") == "delivered" and set(response) - REFUSAL_FIELDS == {
            "kind",
            "content",
            "dispatched_tool",
            "dispatched_arguments",
            "disposition",
        }:
            delivered = response["content"]
            disposition = response["disposition"]
            if isinstance(delivered, str) and disposition in {"admitted", "sealed"}:
                return Reported(delivered, disposition)
        raise NativeProtocolError("native report response has an invalid envelope")

    def abandon(self) -> None:
        if self._pending:
            self._session.abandon()
            self._pending = False

    def close(self) -> None:
        if self._closed:
            return
        self.abandon()
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


class NativeChildSession:
    """Typed handle for one isolated native child trajectory."""

    def __init__(self, child: object, available_tools: set[str]) -> None:
        self._child = child
        self._available_tools = available_tools
        self._pending: Allowed | None = None
        self._finished = False

    def check(self, tool: str, arguments: dict[str, object]) -> CheckResult:
        self._require_live()
        if self._pending is not None:
            raise NativeProtocolError("a native child call is already pending")
        try:
            raw = self._child.check(
                tool,
                json.dumps(arguments, separators=(",", ":"), ensure_ascii=False),
            )
        except appa_agent_python.AppaError as error:
            return Refused(str(error))
        response = NativeSession._decode(raw)
        match response.get("kind"):
            case "blocked":
                blocked = _blocked(response)
                if blocked is not None:
                    return blocked
            case "control" if set(response) == {"kind", "reply", "retry"}:
                reply = response["reply"]
                if isinstance(reply, str):
                    return Control(reply)
            case "allowed" if set(response) == {
                "kind",
                "dispatched_tool",
                "dispatched_arguments",
                "spawn_binding",
            }:
                dispatched_tool = response["dispatched_tool"]
                dispatched_arguments = response["dispatched_arguments"]
                if (
                    isinstance(dispatched_tool, str)
                    and isinstance(dispatched_arguments, dict)
                    and response["spawn_binding"] is None
                ):
                    if dispatched_tool not in self._available_tools:
                        raise NativeProtocolError("native child check dispatched an unavailable Inspect tool")
                    if tool != EXECUTE_REMEDY_PLAN and (dispatched_tool != tool or dispatched_arguments != arguments):
                        raise NativeProtocolError("native child check altered an ordinary Inspect call")
                    allowed = Allowed(dispatched_tool, dispatched_arguments)
                    self._pending = allowed
                    return allowed
        raise NativeProtocolError("native child check response has an invalid envelope")

    def report(self, content: str | None, error: bool = False) -> Reported:
        self._require_live()
        pending = self._pending
        if pending is None:
            raise NativeProtocolError("no native child call is pending")
        # A refused report leaves the call pending, to report again or abandon.
        response = NativeSession._decode(self._child.report(content, error))
        if (withheld := _unsettled(response)) is not None:
            self.abandon()
            return Reported(withheld, "sealed")
        self._pending = None
        if response.get("kind") == "delivered" and set(response) - REFUSAL_FIELDS == {
            "kind",
            "content",
            "dispatched_tool",
            "dispatched_arguments",
            "disposition",
        }:
            delivered = response["content"]
            disposition = response["disposition"]
            if (
                isinstance(delivered, str)
                and disposition in {"admitted", "sealed"}
                and response["dispatched_tool"] == pending.dispatched_tool
                and response["dispatched_arguments"] == pending.dispatched_arguments
            ):
                return Reported(delivered, disposition)
        raise NativeProtocolError("native child report response has an invalid envelope")

    def abandon(self) -> None:
        self._require_live()
        if self._pending is not None:
            self._child.abandon()
            self._pending = None

    def finish(self, value: object | None = None) -> FinishResult:
        self._require_live()
        if self._pending is not None:
            raise NativeProtocolError("cannot finish while a native child call is pending")
        raw = self._child.finish(value)
        response = NativeSession._decode(raw)
        match response.get("kind"):
            # A held return keeps the child live: it may return again.
            case "blocked":
                blocked = _blocked(response)
                if blocked is not None:
                    return blocked
            case "returned" if set(response) == {"kind", "value", "disposition"}:
                returned = response["value"]
                disposition = response["disposition"]
                if (returned is None or isinstance(returned, str)) and disposition in {"crossed", "substituted"}:
                    self._finished = True
                    return Returned(returned, disposition)
        raise NativeProtocolError("native child finish response has an invalid envelope")

    def _require_live(self) -> None:
        if self._finished:
            raise NativeProtocolError("the native child has already finished")
