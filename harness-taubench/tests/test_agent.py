from collections.abc import Iterator

import pytest
from tau2.data_model.message import AssistantMessage, ToolCall, ToolMessage, UserMessage
from tau2.environment.tool import as_tool

from appa_taubench.agent import AppaAgent, drain_stats
from appa_taubench.native import Allowed, Blocked, Reported


def lookup(value: str) -> str:
    """Look up a value.

    Args:
        value: Value to look up.
    """
    return value


class FakeSession:
    decisions: Iterator[Allowed | Blocked]
    instance: "FakeSession"

    def __init__(self, policy, tools, user_prompt) -> None:
        self.user_prompt = user_prompt
        self.reported = []
        self.rounds = 0
        self.closed = False
        self.abandoned = False
        FakeSession.instance = self

    def check(self, tool, arguments):
        return next(self.decisions)

    def report(self, content, error):
        self.reported.append((content, error))
        return Reported(content or "[sealed]", "sealed" if error else "admitted")

    def new_round(self):
        self.rounds += 1

    def close(self):
        self.closed = True


@pytest.fixture(autouse=True)
def clear_stats():
    drain_stats()
    yield
    drain_stats()


def response(tool: str | None, cost: float = 0.1) -> AssistantMessage:
    if tool is None:
        return AssistantMessage.text("done", cost=cost)
    return AssistantMessage(
        role="assistant",
        content=None,
        tool_calls=[ToolCall(id=f"call-{tool}", name=tool, arguments={"value": "one"})],
        cost=cost,
    )


def test_allowed_call_executes_in_taubench_then_reports_before_the_next_completion(monkeypatch) -> None:
    FakeSession.decisions = iter([Allowed("lookup", {"value": "one"})])
    responses = iter([response("lookup"), response(None)])
    monkeypatch.setattr("appa_taubench.agent.FrameworkSession", FakeSession)
    monkeypatch.setattr("appa_taubench.agent.generate", lambda **kwargs: next(responses))

    agent = AppaAgent([as_tool(lookup)], "domain policy", "appa policy", "model")
    state = agent.get_init_state()
    proposed, state = agent.generate_next_message(UserMessage.text("find one"), state)
    assert proposed.tool_calls[0].name == "lookup"

    final, state = agent.generate_next_message(ToolMessage(id="call-lookup", role="tool", content="found one"), state)
    assert final.content == "done"
    assert FakeSession.instance.reported == [("found one", False)]
    assert FakeSession.instance.rounds == 1
    agent.stop()


def test_block_feedback_stays_inside_the_agent_and_only_an_allowed_call_reaches_taubench(monkeypatch) -> None:
    FakeSession.decisions = iter(
        [
            Blocked("need a safer path"),
            Allowed("lookup", {"value": "safe"}),
        ]
    )
    responses = iter([response("lookup", 0.2), response("lookup", 0.3)])
    monkeypatch.setattr("appa_taubench.agent.FrameworkSession", FakeSession)
    monkeypatch.setattr("appa_taubench.agent.generate", lambda **kwargs: next(responses))

    agent = AppaAgent([as_tool(lookup)], "domain policy", "appa policy", "model")
    proposed, _ = agent.generate_next_message(UserMessage.text("find one"), agent.get_init_state())

    assert proposed.tool_calls[0].arguments == {"value": "safe"}
    assert proposed.cost == pytest.approx(0.5)
    assert agent.stats.policy_blocks == 1
    assert agent.stats.allowed == 1
    assert FakeSession.instance.rounds == 1
    agent.stop()
