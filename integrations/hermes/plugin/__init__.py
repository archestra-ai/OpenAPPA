"""OpenAPPA for Hermes Agent: checks every tool call and result against APPA policy.

Speaks the shared hook wire to `appa runtime --adapter hermes` (APPA_HERMES_RUNTIME_URL,
default http://127.0.0.1:8789). The trajectory is the Hermes session; a subagent's calls
are checked on its parent's trajectory. Hermes lets a call run when a hook raises, so
every failure here is returned as a block.
"""

import json
import logging
import os
import threading
import urllib.error
import urllib.request
import uuid
from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any

logger = logging.getLogger(__name__)

DEFAULT_RUNTIME_URL = "http://127.0.0.1:8789"
# Below Hermes' 30 s hook budget, past which it blocks the call itself.
REQUEST_TIMEOUT_SECONDS = 25
UNAVAILABLE = "[appa] Runtime unavailable or unreadable reply. Check APPA_HERMES_RUNTIME_URL and the runtime logs."
WITHHELD = "[appa] Tool result withheld."
GUIDANCE = "OpenAPPA checks tool flows. Follow policy feedback; do not bypass a blocked flow through another tool."


class GateFailure(Exception):
    """A failure whose message is safe to show the model."""


class Event(StrEnum):
    SESSION_START = "session_start"
    PROMPT = "prompt"
    TOOL_CALL = "tool_call"
    TOOL_RESULT = "tool_result"
    TURN_END = "turn_end"


Block = dict[str, str]


def block(message: str) -> Block:
    return {"action": "block", "message": message}


@dataclass
class Runtime:
    base_url: str

    def post(self, event: Event, root: str, **fields: Any) -> dict[str, Any]:
        body = json.dumps({"protocol": 1, "adapter": "hermes", "event": event, "root_id": root, **fields})
        request = urllib.request.Request(
            f"{self.base_url.rstrip('/')}/hook",
            data=body.encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT_SECONDS) as response:
                status, raw = response.status, response.read()
        except urllib.error.HTTPError as error:
            status, raw = error.code, error.read()
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            raise GateFailure(UNAVAILABLE) from error
        try:
            reply = json.loads(raw)
        except json.JSONDecodeError as error:
            raise GateFailure(f"[appa] Invalid runtime reply (HTTP {status}). Check --adapter hermes.") from error
        match reply:
            case {"protocol": 1, "decision": "refuse", "detail": str(detail)}:
                raise GateFailure(f"[appa] {detail}")
            case {"protocol": 1, "decision": str()} if 200 <= status < 300:
                return reply
            case _:
                raise GateFailure(f"[appa] Invalid runtime reply (HTTP {status}). Check --adapter hermes.")


def _text(reply: dict[str, Any], key: str) -> str:
    match reply.get(key):
        case str(value):
            return value
        case _:
            raise GateFailure(f"[appa] Runtime reply is missing {key}.")


@dataclass
class Gate:
    runtime: Runtime
    lock: threading.Lock = field(default_factory=threading.Lock)
    # child session -> parent session, from subagent_start
    parents: dict[str, str] = field(default_factory=dict)
    # task -> root, for calls Hermes reports without a session (execute_code's sandbox)
    task_roots: dict[str, str] = field(default_factory=dict)
    # dispatch key -> (root, call id) of calls the runtime allowed and awaits results for
    allowed: dict[str, tuple[str, str]] = field(default_factory=dict)
    # dispatch keys of calls this gate blocked; Hermes hands the block message over as their result
    blocked: set[str] = field(default_factory=set)

    def root(self, session_id: str, task_id: str) -> str:
        with self.lock:
            match session_id:
                case "":
                    root = self.task_roots.get(task_id)
                    if root is None:
                        raise GateFailure("[appa] A tool call names no Hermes session this plugin has seen.")
                    return root
                case _:
                    root = session_id
                    while root in self.parents:
                        root = self.parents[root]
                    if task_id:
                        self.task_roots[task_id] = root
                    return root

    def is_child(self, session_id: str) -> bool:
        with self.lock:
            return session_id in self.parents

    def on_subagent_start(self, parent_session_id: str = "", child_session_id: str = "", **_: Any) -> None:
        if parent_session_id and child_session_id:
            with self.lock:
                self.parents[child_session_id] = parent_session_id

    def on_pre_llm_call(
        self, session_id: str = "", task_id: str = "", user_message: str = "", parent_session_id: str = "", **_: Any
    ) -> dict[str, str] | None:
        if parent_session_id:
            self.on_subagent_start(parent_session_id=parent_session_id, child_session_id=session_id)
        if self.is_child(session_id):
            return None
        root = self.root(session_id, task_id)
        # Reopening a root keeps its label: the label lives in the runtime, not here.
        started = self.runtime.post(Event.SESSION_START, root)
        prompted = self.runtime.post(Event.PROMPT, root, text=user_message or "")
        if prompted["decision"] != "ack":
            raise GateFailure("[appa] Unexpected prompt decision.")
        match started["decision"]:
            case "context":
                return {"context": _text(started, "text")}
            case "ack":
                return {"context": GUIDANCE}
            case _:
                raise GateFailure("[appa] Unexpected session-start decision.")

    def on_post_llm_call(self, session_id: str = "", task_id: str = "", **_: Any) -> None:
        if self.is_child(session_id):
            return
        root = self.root(session_id, task_id)
        with self.lock:
            self.allowed = {key: value for key, value in self.allowed.items() if value[0] != root}
            self.blocked.clear()
        if self.runtime.post(Event.TURN_END, root)["decision"] != "ack":
            raise GateFailure("[appa] Unexpected turn-end decision.")

    def on_pre_tool_call(
        self,
        tool_name: str = "",
        args: dict[str, Any] | None = None,
        session_id: str = "",
        task_id: str = "",
        tool_call_id: str = "",
        **_: Any,
    ) -> Block | None:
        arguments = args or {}
        key = _dispatch_key(tool_call_id, task_id, tool_name, arguments)
        decision = self._check(tool_name, arguments, session_id, task_id, tool_call_id, key)
        if decision is not None:
            with self.lock:
                self.blocked.add(key)
        return decision

    def _check(
        self, tool_name: str, arguments: dict[str, Any], session_id: str, task_id: str, tool_call_id: str, key: str
    ) -> Block | None:
        try:
            root = self.root(session_id, task_id)
            call_id = tool_call_id or str(uuid.uuid4())
            reply = self.runtime.post(
                Event.TOOL_CALL, root, tool=tool_name, arguments=arguments, call_id=call_id, cwd=os.getcwd()
            )
            match reply["decision"]:
                case "allow_call" | "pass_control":
                    with self.lock:
                        self.allowed[key] = (root, call_id)
                    return None
                case "deny_call":
                    return block(_text(reply, "feedback"))
                case "block":
                    return block(_text(reply, "reason"))
                case _:
                    raise GateFailure("[appa] Unexpected tool-call decision.")
        except GateFailure as error:
            return block(str(error))
        except Exception:
            logger.exception("appa pre_tool_call failed")
            return block(UNAVAILABLE)

    def on_transform_tool_result(
        self,
        tool_name: str = "",
        args: dict[str, Any] | None = None,
        result: Any = None,
        task_id: str = "",
        tool_call_id: str = "",
        status: str = "ok",
        **_: Any,
    ) -> str | None:
        arguments = args or {}
        key = _dispatch_key(tool_call_id, task_id, tool_name, arguments)
        with self.lock:
            dispatched = self.allowed.pop(key, None)
            was_blocked = key in self.blocked
            self.blocked.discard(key)
        match dispatched:
            case None if was_blocked:
                return None
            case None:
                # A result of no call the runtime released never reaches the model.
                return WITHHELD
        root, call_id = dispatched
        text = result if isinstance(result, str) else json.dumps(result)
        outcome = {"status": "failure", "message": text} if status == "error" else {"status": "success", "body": text}
        try:
            reply = self.runtime.post(
                Event.TOOL_RESULT, root, tool=tool_name, arguments=arguments, call_id=call_id, outcome=outcome
            )
            match reply["decision"]:
                case "ack":
                    return None
                case "deliver_value":
                    return _text(reply, "value")
                case "replace_output":
                    return _text(reply, "output")
                case "block":
                    return f"{WITHHELD} {_text(reply, 'reason')}"
                case _:
                    raise GateFailure("[appa] Unexpected tool-result decision.")
        except GateFailure as error:
            return f"{WITHHELD} {error}"
        except Exception:
            logger.exception("appa transform_tool_result failed")
            return f"{WITHHELD} {UNAVAILABLE}"


def _dispatch_key(tool_call_id: str, task_id: str, tool_name: str, arguments: dict[str, Any]) -> str:
    match tool_call_id:
        case "":
            return f"{task_id}\0{tool_name}\0{json.dumps(arguments, sort_keys=True)}"
        case _:
            return tool_call_id


def register(ctx: Any) -> None:
    gate = Gate(Runtime(os.environ.get("APPA_HERMES_RUNTIME_URL", DEFAULT_RUNTIME_URL)))
    ctx.register_hook("subagent_start", gate.on_subagent_start)
    ctx.register_hook("pre_llm_call", gate.on_pre_llm_call)
    ctx.register_hook("post_llm_call", gate.on_post_llm_call)
    ctx.register_hook("pre_tool_call", gate.on_pre_tool_call)
    ctx.register_hook("transform_tool_result", gate.on_transform_tool_result)
