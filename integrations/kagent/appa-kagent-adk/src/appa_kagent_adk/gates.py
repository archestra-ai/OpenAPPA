"""Entrypoint gates for the out-of-band ADK flows.

Two ADK features on the kagent python runtime move a value without a
``FunctionTool`` call, so ``before_tool_callback`` never sees them.
The entrypoint brings each under the tool gate here:

- Code execution runs through a wrapped ``code_executor``. The wrapper
  sends a ``ToolCall`` for the code, runs the inner executor only on
  ``allow_call``, and returns the output through a ``ToolResult``.
- The memory write-back runs through a wrapped ``after_agent_callback``.
  The wrapper sends a ``ToolCall`` for the persist and calls the stock
  callback only on ``allow_call``.

The synthetic tool names are ``appa_code_execution`` and
``appa_memory_persist`` — the spellings a policy's ``[[tool]]`` entries
and mandates address.
"""

from __future__ import annotations

import logging
from typing import Any

import httpx

from . import wire
from .identity import SessionIdentity
from .plugin import AppaFailClosed, AppaPluginKagent

logger = logging.getLogger("appa_kagent_adk.gates")

CODE_EXECUTION_TOOL = "appa_code_execution"
MEMORY_PERSIST_TOOL = "appa_memory_persist"

_STOCK_MEMORY_CALLBACK = "auto_save_session_to_memory_callback"


class SyncHookGate:
    """The synchronous twin of the plugin's transport.

    ADK invokes a code executor synchronously, so its gate cannot
    await. Same wire, same fail-closed contract, one shared identity.
    """

    def __init__(self, runtime_url: str, identity: SessionIdentity, client: httpx.Client | None = None):
        self._hook_url = runtime_url.rstrip("/") + "/hook"
        self._identity = identity
        self._client = client or httpx.Client(timeout=120.0)

    def post(self, event: dict[str, Any]) -> wire.Decision:
        try:
            answer = self._client.post(self._hook_url, json=event)
        except httpx.HTTPError as error:
            raise AppaFailClosed(f"the appa /hook channel is down: {error}") from error
        if answer.status_code != 200:
            raise AppaFailClosed(f"appa answered {answer.status_code}: {answer.text[:500]}")
        try:
            return wire.parse_decision(answer.content)
        except wire.WireError as error:
            raise AppaFailClosed(str(error)) from error

    def ids(self, session: Any) -> tuple[str, str | None]:
        return self._identity.ids(session)


class GatedCodeExecutor:
    """Wraps the agent's code executor so code crosses the tool gate.

    A plain delegating wrapper: ADK reads executor attributes
    (delimiters, retry counts) off this object, and ``__getattr__``
    forwards them to the inner executor unchanged.
    """

    def __init__(self, inner: Any, gate: SyncHookGate):
        self._inner = inner
        self._gate = gate

    def __getattr__(self, name: str) -> Any:
        return getattr(self._inner, name)

    def execute_code(self, invocation_context: Any, code_execution_input: Any) -> Any:
        from google.adk.code_executors.code_execution_utils import CodeExecutionResult

        session = invocation_context.session
        root_id, child_id = self._gate.ids(session)
        arguments = {"code": code_execution_input.code}
        decision = self._gate.post(wire.tool_call(root_id, CODE_EXECUTION_TOOL, arguments, False, child_id))
        if decision.kind == "deny_call":
            return CodeExecutionResult(stdout="", stderr=decision.feedback or "")
        if decision.kind not in ("allow_call", "pass_control"):
            raise AppaFailClosed(f"appa answered the code execution with {decision.detail or decision.kind}")
        result = self._inner.execute_code(invocation_context, code_execution_input)
        outcome = wire.success({"stdout": result.stdout, "stderr": result.stderr})
        answer = self._gate.post(wire.tool_result(root_id, CODE_EXECUTION_TOOL, arguments, outcome, child_id))
        if answer.kind == "ack":
            return result
        if answer.kind == "replace_output":
            return CodeExecutionResult(stdout=answer.output or "", stderr="")
        if answer.kind == "block":
            withheld = f"[appa] the tool result was withheld: {answer.reason}"
            return CodeExecutionResult(stdout="", stderr=withheld)
        raise AppaFailClosed(f"appa answered the code output with {answer.detail or answer.kind}")


def gate_memory_persist(agent: Any, plugin: AppaPluginKagent) -> bool:
    """Wrap the stock memory auto-save callback under the tool gate.

    Returns whether a stock persist callback was found and wrapped. The
    read tools (``load_memory``, ``save_memory``, ``prefetch_memory``)
    stay ordinary tools and already cross the gate.
    """
    callbacks = getattr(agent, "after_agent_callback", None)
    if not callbacks:
        return False
    for index, callback in enumerate(callbacks):
        if getattr(callback, "__name__", "") != _STOCK_MEMORY_CALLBACK:
            continue
        callbacks[index] = _gated_persist(callback, plugin)
        return True
    return False


def _gated_persist(stock: Any, plugin: AppaPluginKagent):
    async def appa_gated_memory_persist(callback_context: Any):
        session = callback_context._invocation_context.session
        arguments = {"session_id": session.id}
        decision = await plugin.gate_synthetic_call(session, MEMORY_PERSIST_TOOL, arguments)
        if decision.kind == "deny_call":
            logger.info("appa denied the memory persist for session %s: %s", session.id, decision.feedback)
            return None
        if decision.kind not in ("allow_call", "pass_control"):
            raise AppaFailClosed(f"appa answered the memory persist with {decision.detail or decision.kind}")
        returned = await stock(callback_context)
        await plugin.report_synthetic_result(session, MEMORY_PERSIST_TOOL, arguments, {"persisted": True})
        return returned

    return appa_gated_memory_persist
