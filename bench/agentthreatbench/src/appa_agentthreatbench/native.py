"""Typed adapter over the framework-owned ``appa_agent_python`` lifecycle."""

from __future__ import annotations

import json
import re
from collections.abc import Callable
from dataclasses import dataclass
from typing import Literal

import appa_agent_python
from inspect_ai.tool import Tool, ToolDef

BINDING_IDENTITY: str = appa_agent_python.BINDING_IDENTITY
EXECUTE_REMEDY_PLAN = "execute_remedy_plan"
EXECUTABLE_REMEDY = re.compile(r"^(?:Continue:|Option: )", re.MULTILINE)


class NativeProtocolError(RuntimeError):
    """The native extension returned an invalid response envelope."""


@dataclass(frozen=True)
class Unestablished:
    """One value the blocked call reads that no registered cast can label."""

    value: int
    tool: str | None
    dimensions: tuple[str, ...]


@dataclass(frozen=True)
class Blocked:
    feedback: str
    recoverable: bool = False
    unestablished: tuple[Unestablished, ...] = ()


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


type CheckResult = Blocked | Control | Allowed
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


def _blocked(response: dict[str, object], has_remedy: Callable[[str], bool]) -> Blocked | None:
    """The blocked envelope: `feedback` plus, on a call decision, the values no cast reaches."""
    if set(response) - {"unestablished"} != {"kind", "feedback"}:
        return None
    feedback = response["feedback"]
    if not isinstance(feedback, str):
        return None
    entries = response.get("unestablished", [])
    if not isinstance(entries, list):
        return None
    unestablished: list[Unestablished] = []
    for entry in entries:
        match entry:
            case {"value": int() as value, "tool": str() | None as tool, "dimensions": list() as dimensions} if all(
                dimension in ("trust", "audience") for dimension in dimensions
            ):
                unestablished.append(Unestablished(value, tool, tuple(dimensions)))
            case _:
                return None
    return Blocked(feedback, has_remedy(feedback), tuple(unestablished))


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
                blocked = _blocked(response, self._has_remedy)
                if blocked is not None:
                    return blocked, None
            case "control" if set(response) == {"kind", "reply"}:
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
                if return_schema is not None:
                    expected_arguments["return_schema"] = return_schema
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
        response = self._decode(
            self._session.check(
                tool,
                json.dumps(arguments, separators=(",", ":"), ensure_ascii=False),
            )
        )
        match response.get("kind"):
            case "blocked":
                blocked = _blocked(response, self._has_remedy)
                if blocked is not None:
                    return blocked
            case "control" if set(response) == {"kind", "reply"}:
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
        try:
            raw = self._session.report(content, error)
        finally:
            # Rust consumes the pending handle before it attempts admission.
            self._pending = False
        response = self._decode(raw)
        if response.get("kind") == "delivered" and set(response) == {
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

    @staticmethod
    def _has_remedy(feedback: str) -> bool:
        return EXECUTABLE_REMEDY.search(feedback) is not None


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
        response = NativeSession._decode(
            self._child.check(
                tool,
                json.dumps(arguments, separators=(",", ":"), ensure_ascii=False),
            )
        )
        match response.get("kind"):
            case "blocked":
                blocked = _blocked(response, NativeSession._has_remedy)
                if blocked is not None:
                    return blocked
            case "control" if set(response) == {"kind", "reply"}:
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
        try:
            raw = self._child.report(content, error)
        finally:
            # Rust consumes the pending handle before it attempts admission.
            self._pending = None
        response = NativeSession._decode(raw)
        if response.get("kind") == "delivered" and set(response) == {
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
        self._finished = True
        response = NativeSession._decode(raw)
        match response.get("kind"):
            case "blocked":
                blocked = _blocked(response, NativeSession._has_remedy)
                if blocked is not None:
                    return blocked
            case "returned" if set(response) == {"kind", "value", "disposition"}:
                returned = response["value"]
                disposition = response["disposition"]
                if (returned is None or isinstance(returned, str)) and disposition in {"crossed", "substituted"}:
                    return Returned(returned, disposition)
        raise NativeProtocolError("native child finish response has an invalid envelope")

    def _require_live(self) -> None:
        if self._finished:
            raise NativeProtocolError("the native child has already finished")
