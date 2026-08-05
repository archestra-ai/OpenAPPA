import json
from collections.abc import Iterator

import pytest
from tau2.data_model.message import AssistantMessage, ToolCall, ToolMessage, UserMessage
from tau2.environment.tool import as_tool

from appa_taubench import AGENT_PROMPT_PROFILES
from appa_taubench.agent import AppaAgent, drain_stats
from appa_taubench.knowledge import DISCOVERABLE_WRAPPER
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

    def __init__(self, policy, tools, user_prompt, logical_tools=None) -> None:
        self.user_prompt = user_prompt
        self.logical_tools = logical_tools or []
        self.reported = []
        self.rounds = 0
        self.closed = False
        self.abandoned = False
        FakeSession.instance = self

    def check(self, tool, arguments):
        return next(self.decisions)

    def logical_call(self, tool, arguments):
        if tool != DISCOVERABLE_WRAPPER:
            return tool, arguments
        return arguments["agent_tool_name"], json.loads(arguments.get("arguments", "{}"))

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


def response(tool: str | None, cost: float = 0.1, arguments=None) -> AssistantMessage:
    if tool is None:
        return AssistantMessage.text("done", cost=cost)
    return AssistantMessage(
        role="assistant",
        content=None,
        tool_calls=[
            ToolCall(
                id=f"call-{tool}",
                name=tool,
                arguments=arguments or {"value": "one"},
            )
        ],
        cost=cost,
    )


def test_verification_recovery_profile_is_an_explicit_system_prompt_addendum() -> None:
    standard = AppaAgent([as_tool(lookup)], "domain policy", "appa policy", "model")
    chaos = AppaAgent(
        [as_tool(lookup)],
        "domain policy",
        "appa policy",
        "model",
        agent_prompt_profile="verification-recovery-chaos",
    )

    assert chaos.system_prompt == (
        f"{standard.system_prompt}\n\n{AGENT_PROMPT_PROFILES['verification-recovery-chaos']}"
    )
    with pytest.raises(ValueError, match="unknown agent prompt profile"):
        AppaAgent(
            [as_tool(lookup)],
            "domain policy",
            "appa policy",
            "model",
            agent_prompt_profile="invalid",
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
            Blocked("need a safer path", recoverable=True),
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


def test_logical_dispatch_and_hidden_retry_are_correlated_in_the_audit(monkeypatch, tmp_path) -> None:
    wrapped_arguments = {
        "agent_tool_name": "write_record",
        "arguments": '{"record_id":"safe"}',
    }
    FakeSession.decisions = iter(
        [
            Blocked("use the remedy", recoverable=True),
            Allowed(DISCOVERABLE_WRAPPER, wrapped_arguments),
        ]
    )
    responses = iter(
        [
            response(DISCOVERABLE_WRAPPER, 0.2, wrapped_arguments),
            response("execute_remedy_plan", 0.3, {"plan_id": "remedy-0"}),
            response(None),
        ]
    )
    monkeypatch.setattr("appa_taubench.agent.FrameworkSession", FakeSession)
    monkeypatch.setattr("appa_taubench.agent.generate", lambda **kwargs: next(responses))

    agent = AppaAgent(
        [as_tool(lookup)],
        "domain policy",
        "appa policy",
        "model",
        llm_args={"seed": 42},
        audit_dir=str(tmp_path),
        task_id="task-1",
    )
    proposed, state = agent.generate_next_message(UserMessage.text("write it"), agent.get_init_state())
    assert proposed.tool_calls[0].name == DISCOVERABLE_WRAPPER
    assert proposed.cost == pytest.approx(0.5)
    final, _ = agent.generate_next_message(
        ToolMessage(id="call-execute_remedy_plan", role="tool", content="written"),
        state,
    )
    assert final.content == "done"
    agent.stop()

    [audit_path] = list(tmp_path.glob("*.json"))
    audit = json.loads(audit_path.read_text())
    assert audit["task_id"] == "task-1"
    assert audit["model_args"] == {"seed": 42}
    assert audit["stats"]["policy_blocks"] == 1
    assert audit["stats"]["completions"] == 3
    policy_events = [event for event in audit["events"] if event["kind"].startswith("policy_")]
    assert policy_events[0]["policy_tool"] == "write_record"
    assert policy_events[0]["tool_call_id"] == f"call-{DISCOVERABLE_WRAPPER}"
    assert policy_events[1]["tool_call_id"] == "call-execute_remedy_plan"
    assert policy_events[1]["dispatched_tool"] == DISCOVERABLE_WRAPPER
    result_event = next(event for event in audit["events"] if event["kind"] == "tool_result")
    assert result_event["tool_call_id"] == "call-execute_remedy_plan"
    assert result_event["original_content"] == "written"


def test_mismatched_tau_result_still_closes_the_session_and_audit(monkeypatch, tmp_path) -> None:
    FakeSession.decisions = iter([Allowed("lookup", {"value": "one"})])
    monkeypatch.setattr("appa_taubench.agent.FrameworkSession", FakeSession)
    monkeypatch.setattr("appa_taubench.agent.generate", lambda **kwargs: response("lookup"))
    agent = AppaAgent(
        [as_tool(lookup)],
        "domain policy",
        "appa policy",
        "model",
        audit_dir=str(tmp_path),
        task_id="task-1",
    )
    _, state = agent.generate_next_message(UserMessage.text("find one"), agent.get_init_state())

    with pytest.raises(ValueError, match="does not match"):
        agent.generate_next_message(ToolMessage(id="wrong", role="tool", content="found"), state)
    agent.stop()

    assert FakeSession.instance.closed
    assert not agent.pending
    assert len(list(tmp_path.glob("*.json"))) == 1


def test_irrecoverable_block_is_terminal_without_hidden_retry(monkeypatch, tmp_path) -> None:
    FakeSession.decisions = iter([Blocked("no remedy")])
    calls = []
    monkeypatch.setattr("appa_taubench.agent.FrameworkSession", FakeSession)
    monkeypatch.setattr(
        "appa_taubench.agent.generate",
        lambda **kwargs: calls.append(kwargs) or response("lookup", 0.2),
    )
    agent = AppaAgent(
        [as_tool(lookup)],
        "domain policy",
        "appa policy",
        "model",
        audit_dir=str(tmp_path),
    )

    final, _ = agent.generate_next_message(UserMessage.text("find one"), agent.get_init_state())
    agent.stop()

    assert final.content == "I cannot complete that request because OpenAPPA refused the proposed action."
    assert final.cost == pytest.approx(0.2)
    assert len(calls) == 1
    [audit_path] = list(tmp_path.glob("*.json"))
    events = json.loads(audit_path.read_text())["events"]
    assert next(event for event in events if event["kind"] == "policy_block")["recoverable"] is False
    assert next(event for event in events if event["kind"] == "terminal_refusal")["reason"] == "no_remedy_available"


def test_model_cannot_claim_success_after_a_failed_tau_result(monkeypatch) -> None:
    FakeSession.decisions = iter([Allowed("lookup", {"value": "one"})])
    responses = iter(
        [
            response("lookup"),
            AssistantMessage.text("That succeeded!", cost=0.2),
            AssistantMessage.text("I can still answer your question.", cost=0.1),
        ]
    )
    monkeypatch.setattr("appa_taubench.agent.FrameworkSession", FakeSession)
    monkeypatch.setattr("appa_taubench.agent.generate", lambda **kwargs: next(responses))
    agent = AppaAgent([as_tool(lookup)], "domain policy", "appa policy", "model")
    proposed, state = agent.generate_next_message(UserMessage.text("find one"), agent.get_init_state())

    final, _ = agent.generate_next_message(
        ToolMessage(
            id=proposed.tool_calls[0].id,
            role="tool",
            content="lookup failed",
            error=True,
        ),
        state,
    )
    assert final.content == "I could not complete that request because the attempted action did not succeed."
    assert final.cost == pytest.approx(0.2)

    next_turn, _ = agent.generate_next_message(UserMessage.text("Can you explain?"), state)
    agent.stop()

    assert next_turn.content == "I can still answer your question."
