from agentdojo.functions_runtime import FunctionCall, FunctionsRuntime
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
    finally:
        executor.close()
