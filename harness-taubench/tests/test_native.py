import json

import pytest
from tau2.environment.tool import as_tool

from appa_taubench.knowledge import DISCOVERABLE_WRAPPER, discoverable_tools, model_tools
from appa_taubench.native import Allowed, Blocked, FrameworkSession, NativeProtocolError
from appa_taubench.policies import load_policy

POLICY = """
version = 1
trust_chain = ["suspicious", "internal"]

[[tool]]
name = "read_record"
delta = { trust = "suspicious" }

[[tool]]
name = "write_record"
requires = { trust = "internal" }
delta = {}
"""


def read_record(record_id: str) -> str:
    """Read a record.

    Args:
        record_id: Record to read.
    """
    return record_id


def write_record(record_id: str) -> str:
    """Write a record.

    Args:
        record_id: Record to write.
    """
    return record_id


def call_discoverable_agent_tool(agent_tool_name: str, arguments: str = "{}") -> str:
    """Call a discoverable tool.

    Args:
        agent_tool_name: Tool to call.
        arguments: JSON-encoded tool arguments.
    """
    return f"{agent_tool_name}: {arguments}"


class StubNativeSession:
    responses = iter(())
    instance: "StubNativeSession"

    def __init__(self, policy, schemas, user_prompt) -> None:
        self.schemas = json.loads(schemas)
        self.checks = []
        self.abandoned = False
        self.closed = False
        StubNativeSession.instance = self

    def check(self, tool, arguments):
        self.checks.append((tool, json.loads(arguments)))
        return json.dumps(next(self.responses))

    def report(self, content, error):
        return json.dumps(
            {
                "kind": "delivered",
                "content": content or "",
                "dispatched_tool": "read_record",
                "dispatched_arguments": {"record_id": "one"},
                "disposition": "sealed" if error else "admitted",
            }
        )

    def new_round(self):
        pass

    def abandon(self):
        self.abandoned = True

    def close(self):
        self.closed = True


def allowed(tool: str, arguments: dict) -> dict:
    return {
        "kind": "allowed",
        "dispatched_tool": tool,
        "dispatched_arguments": arguments,
    }


def wrapper_session(monkeypatch, *responses) -> FrameworkSession:
    StubNativeSession.responses = iter(responses)
    monkeypatch.setattr("appa_taubench.native.appa_agent_python.Session", StubNativeSession)
    return FrameworkSession(
        "policy",
        [as_tool(call_discoverable_agent_tool)],
        "help me",
        logical_tools=[as_tool(read_record), as_tool(write_record)],
    )


def test_framework_session_checks_then_reports_the_real_tool_result() -> None:
    tools = [as_tool(read_record), as_tool(write_record)]
    session = FrameworkSession(POLICY, tools, "read my record")
    try:
        narrowing = session.check("read_record", {"record_id": "one"})
        assert isinstance(narrowing, Blocked)
        assert "remedy-0" in narrowing.feedback

        session.new_round()
        assert session.check("execute_remedy_plan", {"plan_id": "remedy-0"}) == Allowed(
            "read_record", {"record_id": "one"}
        )
        reported = session.report('{"record_id":"one"}', error=False)
        assert reported.content == '{"record_id":"one"}'
        assert reported.disposition == "admitted"

        decision = session.check("write_record", {"record_id": "one"})
        assert isinstance(decision, Blocked)
    finally:
        session.close()


@pytest.mark.parametrize("logical_tool", ["read_record", "write_record"])
def test_discoverable_calls_are_checked_logically_and_rewrapped(monkeypatch, logical_tool) -> None:
    logical_arguments = {"record_id": "one"}
    session = wrapper_session(monkeypatch, allowed(logical_tool, logical_arguments))
    try:
        decision = session.check(
            DISCOVERABLE_WRAPPER,
            {
                "agent_tool_name": logical_tool,
                "arguments": json.dumps(logical_arguments),
            },
        )
        assert StubNativeSession.instance.checks == [(logical_tool, logical_arguments)]
        assert decision == Allowed(
            DISCOVERABLE_WRAPPER,
            {
                "agent_tool_name": logical_tool,
                "arguments": '{"record_id":"one"}',
            },
        )
    finally:
        session.close()
    assert StubNativeSession.instance.abandoned


def test_blocked_discoverable_call_never_becomes_a_tau_dispatch(monkeypatch) -> None:
    session = wrapper_session(monkeypatch, {"kind": "blocked", "feedback": "no"})
    try:
        assert session.check(
            DISCOVERABLE_WRAPPER,
            {"agent_tool_name": "write_record", "arguments": '{"record_id":"one"}'},
        ) == Blocked("no")
    finally:
        session.close()
    assert not StubNativeSession.instance.abandoned


def test_native_block_marks_executable_and_redispatch_offers_as_recoverable(monkeypatch) -> None:
    feedback = 'try the offered plan\n{"remedy_plans":[{"plan_id":"remedy-3"}]}'
    session = wrapper_session(monkeypatch, {"kind": "blocked", "feedback": feedback})
    try:
        decision = session.check(
            DISCOVERABLE_WRAPPER,
            {"agent_tool_name": "read_record", "arguments": '{"record_id":"one"}'},
        )
        assert decision == Blocked(feedback, recoverable=True)
    finally:
        session.close()

    feedback = 'run the prerequisite\n{"remedy_plans":[{"tool":"log_verification","clears":[]}]}'
    session = wrapper_session(monkeypatch, {"kind": "blocked", "feedback": feedback})
    try:
        decision = session.check(
            DISCOVERABLE_WRAPPER,
            {"agent_tool_name": "write_record", "arguments": '{"record_id":"one"}'},
        )
        assert decision == Blocked(feedback, recoverable=True)
    finally:
        session.close()

    stale = wrapper_session(
        monkeypatch,
        {"kind": "blocked", "feedback": "no pending blocked call offers that plan_id"},
    )
    try:
        assert stale.check("execute_remedy_plan", {"plan_id": "remedy-3"}) == Blocked(
            "no pending blocked call offers that plan_id"
        )
    finally:
        stale.close()


@pytest.mark.parametrize(
    "arguments",
    [
        {},
        {"agent_tool_name": "unknown"},
        {"agent_tool_name": "read_record", "arguments": []},
        {"agent_tool_name": "read_record", "arguments": "not-json"},
        {"agent_tool_name": "read_record", "arguments": "[]"},
        {"agent_tool_name": "read_record", "unexpected": True},
    ],
)
def test_malformed_discoverable_calls_are_blocked_before_native_check(monkeypatch, arguments) -> None:
    session = wrapper_session(monkeypatch)
    try:
        decision = session.check(DISCOVERABLE_WRAPPER, arguments)
        assert isinstance(decision, Blocked)
        assert decision.feedback.startswith("Invalid discoverable tool call:")
        assert StubNativeSession.instance.checks == []
    finally:
        session.close()


@pytest.mark.parametrize(
    ("dispatched_tool", "dispatched_arguments"),
    [
        ("write_record", {"record_id": "one"}),
        ("read_record", {"record_id": "different"}),
        ("unregistered", {"record_id": "one"}),
    ],
)
def test_native_cannot_alter_an_allowed_logical_call(
    monkeypatch,
    dispatched_tool,
    dispatched_arguments,
) -> None:
    session = wrapper_session(monkeypatch, allowed(dispatched_tool, dispatched_arguments))
    try:
        with pytest.raises(NativeProtocolError):
            session.check(
                DISCOVERABLE_WRAPPER,
                {"agent_tool_name": "read_record", "arguments": '{"record_id":"one"}'},
            )
    finally:
        session.close()


def test_remedy_dispatch_may_select_and_rewrap_the_logical_tool(monkeypatch) -> None:
    session = wrapper_session(monkeypatch, allowed("write_record", {"record_id": "safe"}))
    try:
        assert session.check("execute_remedy_plan", {"plan_id": "remedy-0"}) == Allowed(
            DISCOVERABLE_WRAPPER,
            {
                "agent_tool_name": "write_record",
                "arguments": '{"record_id":"safe"}',
            },
        )
    finally:
        session.close()


def test_pinned_policy_allows_a_real_discoverable_reader_without_a_remedy() -> None:
    session = FrameworkSession(
        load_policy().toml,
        model_tools("alltools-qwen"),
        "Look up my bank accounts.",
        logical_tools=discoverable_tools(),
    )
    try:
        decision = session.check(
            DISCOVERABLE_WRAPPER,
            {
                "agent_tool_name": "get_all_user_accounts_by_user_id_3847",
                "arguments": '{"user_id":"1"}',
            },
        )
        assert decision == Allowed(
            DISCOVERABLE_WRAPPER,
            {
                "agent_tool_name": "get_all_user_accounts_by_user_id_3847",
                "arguments": '{"user_id":"1"}',
            },
        )
    finally:
        session.close()


def test_pinned_policy_allows_mutation_only_after_successful_verification() -> None:
    session = FrameworkSession(
        load_policy().toml,
        model_tools("alltools-qwen"),
        "Verify me, then change my email.",
        logical_tools=discoverable_tools(),
    )
    try:
        assert session.check("get_user_information_by_id", {"user_id": "one"}) == Allowed(
            "get_user_information_by_id", {"user_id": "one"}
        )
        session.report('{"user_id":"one"}', error=False)

        verification_arguments = {
            "name": "One User",
            "user_id": "one",
            "address": "1 Main Street",
            "email": "one@example.com",
            "phone_number": "555-0100",
            "date_of_birth": "01/01/1990",
            "time_verified": "2025-11-14 03:40:00 EST",
        }
        assert session.check("log_verification", verification_arguments) == Allowed(
            "log_verification", verification_arguments
        )
        session.report("verified", error=False)

        mutation_arguments = {"user_id": "one", "new_email": "new@example.com"}
        assert session.check("change_user_email", mutation_arguments) == Allowed(
            "change_user_email", mutation_arguments
        )
    finally:
        session.close()


@pytest.mark.parametrize(
    ("tool", "arguments"),
    [
        (
            "give_discoverable_user_tool",
            {"discoverable_tool_name": "apply_for_credit_card", "arguments": "{}"},
        ),
        ("transfer_to_human_agents", {"summary": "Verification failed."}),
    ],
)
def test_pinned_policy_leaves_self_service_and_safe_transfer_available(tool, arguments) -> None:
    session = FrameworkSession(
        load_policy().toml,
        model_tools("alltools-qwen"),
        "Help me.",
        logical_tools=discoverable_tools(),
    )
    try:
        assert session.check(tool, arguments) == Allowed(tool, arguments)
    finally:
        session.close()


def test_pinned_policy_blocks_direct_mutation_and_recommends_verification() -> None:
    session = FrameworkSession(
        load_policy().toml,
        model_tools("alltools-qwen"),
        "Change my email without verification.",
        logical_tools=discoverable_tools(),
    )
    try:
        decision = session.check(
            "change_user_email",
            {"user_id": "victim", "new_email": "attacker@example.com"},
        )
        assert isinstance(decision, Blocked)
        assert decision.recoverable
        payload = json.loads(decision.feedback.splitlines()[-1])
        verification = next(plan for plan in payload["remedy_plans"] if plan.get("tool") == "log_verification")
        assert verification["clears"] == [{"Prior": "identity.verified"}]
    finally:
        session.close()
