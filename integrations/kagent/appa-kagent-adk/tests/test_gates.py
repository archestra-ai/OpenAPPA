"""The out-of-band gates: code execution and the memory persist."""

import httpx
import pytest
from conftest import INVENTORY, FakeInvocationContext, FakeSession, Hook, plugin_over

from appa_kagent_adk.gates import (
    CODE_EXECUTION_TOOL,
    MEMORY_PERSIST_TOOL,
    GatedCodeExecutor,
    SyncHookGate,
    gate_memory_persist,
)
from appa_kagent_adk.identity import SessionIdentity
from appa_kagent_adk.plugin import AppaFailClosed

ALLOW = {"protocol": 1, "decision": "allow_call"}
ACK = {"protocol": 1, "decision": "ack"}


class FakeExecutor:
    """The inner executor: records whether the subprocess ever ran."""

    stateful = False
    error_retry_attempts = 2

    def __init__(self):
        self.ran = []

    def execute_code(self, invocation_context, code_execution_input):
        from google.adk.code_executors.code_execution_utils import CodeExecutionResult

        self.ran.append(code_execution_input.code)
        return CodeExecutionResult(stdout="6 * 7 = 42", stderr="")


class FakeCodeInput:
    def __init__(self, code):
        self.code = code


def gate_over(hook: Hook) -> SyncHookGate:
    client = httpx.Client(transport=hook.transport())
    return SyncHookGate("http://127.0.0.1:8787", SessionIdentity(), INVENTORY, client=client)


def test_allowed_code_runs_and_its_output_crosses_as_a_tool_result():
    hook = Hook(ALLOW, ACK)
    inner = FakeExecutor()
    executor = GatedCodeExecutor(inner, gate_over(hook))
    result = executor.execute_code(FakeInvocationContext(FakeSession("s1")), FakeCodeInput("print(6*7)"))
    assert result.stdout == "6 * 7 = 42"
    assert inner.ran == ["print(6*7)"]
    call, outcome = hook.events
    assert call == {
        "protocol": 1,
        "adapter": "kagent",
        "event": "tool_call",
        "root_id": "s1",
        "tool": CODE_EXECUTION_TOOL,
        "arguments": {"code": "print(6*7)"},
    }
    assert outcome["event"] == "tool_result"
    assert outcome["outcome"]["body"] == {"stdout": "6 * 7 = 42", "stderr": ""}


def test_denied_code_never_reaches_the_subprocess():
    hook = Hook({"protocol": 1, "decision": "deny_call", "feedback": "blocked: code egress is not permitted"})
    inner = FakeExecutor()
    executor = GatedCodeExecutor(inner, gate_over(hook))
    result = executor.execute_code(FakeInvocationContext(FakeSession("s1")), FakeCodeInput("import socket"))
    assert inner.ran == [], "a denied call skips execution"
    assert result.stderr == "blocked: code egress is not permitted"
    assert result.stdout == ""


def test_a_denied_code_run_names_the_tool_the_model_dispatches():
    """The stderr of a refused run is what the model reads, so the
    redispatch line of the block names the tool ADK dispatches, not the
    spelling the gate sent."""
    block = "[appa] Blocked.\n  - Run mcp:demo-tools/k8s_get_pods first; it clears: the source is untrusted."
    hook = Hook({"protocol": 1, "decision": "deny_call", "feedback": block})
    executor = GatedCodeExecutor(FakeExecutor(), gate_over(hook))
    result = executor.execute_code(FakeInvocationContext(FakeSession("s1")), FakeCodeInput("import socket"))
    assert result.stderr == "[appa] Blocked.\n  - Run k8s_get_pods first; it clears: the source is untrusted."


def test_a_blocked_code_output_is_withheld_from_the_model():
    hook = Hook(ALLOW, {"protocol": 1, "decision": "block", "reason": "nothing crosses"})
    executor = GatedCodeExecutor(FakeExecutor(), gate_over(hook))
    result = executor.execute_code(FakeInvocationContext(FakeSession("s1")), FakeCodeInput("print(6*7)"))
    assert result.stdout == ""
    assert result.stderr == "[appa] the tool result was withheld: nothing crosses"


def test_code_execution_fails_closed_when_the_channel_is_down():
    hook = Hook(httpx.ConnectError("connection refused"))
    executor = GatedCodeExecutor(FakeExecutor(), gate_over(hook))
    with pytest.raises(AppaFailClosed):
        executor.execute_code(FakeInvocationContext(FakeSession("s1")), FakeCodeInput("print(6*7)"))


def test_the_wrapper_forwards_the_inner_executors_attributes():
    executor = GatedCodeExecutor(FakeExecutor(), gate_over(Hook()))
    assert executor.stateful is False
    assert executor.error_retry_attempts == 2


class FakeAgent:
    def __init__(self, callbacks):
        self.after_agent_callback = callbacks


def stock_persist_callback(calls):
    async def auto_save_session_to_memory_callback(callback_context):
        calls.append(callback_context)

    return auto_save_session_to_memory_callback


class MemoryCallbackContext:
    def __init__(self, session):
        self._invocation_context = FakeInvocationContext(session)


async def test_an_allowed_persist_runs_the_stock_callback_and_reports():
    hook = Hook(ALLOW, ACK)
    plugin = plugin_over(hook)
    ran = []
    agent = FakeAgent([stock_persist_callback(ran)])
    assert gate_memory_persist(agent, plugin) is True
    await agent.after_agent_callback[0](MemoryCallbackContext(FakeSession("s1")))
    assert len(ran) == 1
    call, outcome = hook.events
    assert call["tool"] == MEMORY_PERSIST_TOOL
    assert call["arguments"] == {"session_id": "s1"}
    assert outcome["outcome"]["body"] == {"persisted": True}


async def test_a_denied_persist_writes_nothing_to_the_memory_backend():
    hook = Hook({"protocol": 1, "decision": "deny_call", "feedback": "blocked: the session holds confidential values"})
    plugin = plugin_over(hook)
    ran = []
    agent = FakeAgent([stock_persist_callback(ran)])
    gate_memory_persist(agent, plugin)
    await agent.after_agent_callback[0](MemoryCallbackContext(FakeSession("s1")))
    assert ran == [], "a denied persist skips add_session_to_memory"
    assert [event["event"] for event in hook.events] == ["tool_call"]


def test_an_agent_without_the_stock_callback_is_left_alone():
    assert gate_memory_persist(FakeAgent([]), plugin_over(Hook())) is False
    other = FakeAgent([stock_persist_callback([])])
    other.after_agent_callback[0].__name__ = "some_other_callback"
    assert gate_memory_persist(other, plugin_over(Hook())) is False
