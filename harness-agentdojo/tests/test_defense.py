import os

from agentdojo.functions_runtime import FunctionCall, FunctionsRuntime
from agentdojo.task_suite.task_suite import functions_stack_trace_from_messages
from agentdojo.types import (
    ChatAssistantMessage,
    ChatUserMessage,
    text_content_block_from_string,
)

from appa_dojo.defense import (
    EXECUTE_REMEDY_PLAN,
    AppaRuntimeSetup,
    AppaToolsExecutor,
)
from appa_dojo.pipeline import build_pipeline
from appa_dojo.policies import Policy

POLICY = """
version = 1
trust_chain = ["suspicious", "internal"]

[[tool]]
name = "read_external"
delta = { trust = "suspicious" }

[[tool]]
name = "send_email"
effects = ["egress"]
requires = { trust = "internal" }
delta = {}
"""


def test_executor_runs_an_accepted_read_but_never_dispatches_the_sink() -> None:
    executed: list[str] = []

    def read_external() -> str:
        """Read third-party content."""
        executed.append("read_external")
        return "external content"

    def send_email(to: str) -> str:
        """Send an email.

        :param to: Recipient email address.
        """
        executed.append("send_email")
        return "sent"

    runtime = FunctionsRuntime()
    runtime.register_function(read_external)
    runtime.register_function(send_email)
    query = "read and send"
    user = ChatUserMessage(role="user", content=[text_content_block_from_string(query)])
    setup = AppaRuntimeSetup()
    _, runtime, env, messages, extra_args = setup.query(query, runtime, messages=[user])
    assert EXECUTE_REMEDY_PLAN in runtime.functions

    executor = AppaToolsExecutor(POLICY)
    try:
        read_call = FunctionCall(function="read_external", args={}, id="call-read")
        assistant = ChatAssistantMessage(role="assistant", content=None, tool_calls=[read_call])
        _, runtime, env, messages, extra_args = executor.query(
            query,
            runtime,
            env,
            [*messages, assistant],
            extra_args,
        )
        assert executed == []
        assert messages[-1]["role"] == "tool"
        assert messages[-1]["error"] is not None

        remedy_call = FunctionCall(
            function=EXECUTE_REMEDY_PLAN,
            args={"plan_id": "remedy-0"},
            id="call-remedy",
        )
        assistant = ChatAssistantMessage(role="assistant", content=None, tool_calls=[remedy_call])
        _, runtime, env, messages, extra_args = executor.query(
            query,
            runtime,
            env,
            [*messages, assistant],
            extra_args,
        )
        assert executed == ["read_external"]
        assert messages[-1]["error"] is None
        assert messages[-1]["content"] == [text_content_block_from_string("external content")]

        sink_call = FunctionCall(
            function="send_email",
            args={"to": "attacker@example.com"},
            id="call-send",
        )
        assistant = ChatAssistantMessage(role="assistant", content=None, tool_calls=[sink_call])
        _, _, _, messages, _ = executor.query(
            query,
            runtime,
            env,
            [*messages, assistant],
            extra_args,
        )
        assert executed == ["read_external"]
        assert messages[-1]["role"] == "tool"
        assert messages[-1]["error"] is not None
        projected = executor.finish_episode(messages)
        trace = functions_stack_trace_from_messages(projected)
        assert [(call.function, call.args) for call in trace] == [("read_external", {})]
    finally:
        executor.close()


def test_trace_projection_uses_occurrence_order_when_call_ids_repeat() -> None:
    def first() -> str:
        """Run the first tool."""
        return "first"

    def second() -> str:
        """Run the second tool."""
        return "second"

    runtime = FunctionsRuntime()
    runtime.register_function(first)
    runtime.register_function(second)
    query = "run both"
    user = ChatUserMessage(role="user", content=[text_content_block_from_string(query)])
    _, runtime, env, messages, extra_args = AppaRuntimeSetup().query(query, runtime, messages=[user])
    executor = AppaToolsExecutor(
        'version = 1\n[[tool]]\nname = "first"\ndelta = {}\n[[tool]]\nname = "second"\ndelta = {}\n'
    )
    try:
        for function in ["first", "second"]:
            assistant = ChatAssistantMessage(
                role="assistant",
                content=None,
                tool_calls=[FunctionCall(function=function, args={}, id="reused")],
            )
            _, runtime, env, messages, extra_args = executor.query(
                query,
                runtime,
                env,
                [*messages, assistant],
                extra_args,
            )

        trace = functions_stack_trace_from_messages(executor.finish_episode(messages))
        assert [(call.function, call.id) for call in trace] == [("first", "reused"), ("second", "reused")]
    finally:
        executor.close()


def test_trace_projection_excludes_a_trailing_unexecuted_tool_batch() -> None:
    executor = AppaToolsExecutor("version = 1\n")
    executed = FunctionCall(function="first", args={}, id="executed")
    trailing = FunctionCall(function="second", args={}, id="unexecuted")
    executor._trace_projection.append((executed.id, executed))
    messages = [
        ChatAssistantMessage(role="assistant", content=None, tool_calls=[executed]),
        ChatAssistantMessage(role="assistant", content=None, tool_calls=[trailing]),
    ]
    try:
        trace = functions_stack_trace_from_messages(executor.finish_episode(messages))
        assert [(call.function, call.id) for call in trace] == [("first", "executed")]
    finally:
        executor.close()


def test_none_pipeline_keeps_the_stock_tools_executor() -> None:
    from agentdojo.agent_pipeline import ToolsExecutionLoop, ToolsExecutor

    previous_key = os.environ.get("OPENROUTER_API_KEY")
    os.environ["OPENROUTER_API_KEY"] = "test-key"
    try:
        policy = Policy(name="open", toml="version = 1", tools=frozenset())
        built = build_pipeline("test-model", "none", policy)
    finally:
        if previous_key is None:
            del os.environ["OPENROUTER_API_KEY"]
        else:
            os.environ["OPENROUTER_API_KEY"] = previous_key

    assert built.executor is None
    assert built.pipeline.name == "test-model-none"
    loops = [element for element in built.pipeline.elements if isinstance(element, ToolsExecutionLoop)]
    assert len(loops) == 1
    assert any(type(element) is ToolsExecutor for element in loops[0].elements)
