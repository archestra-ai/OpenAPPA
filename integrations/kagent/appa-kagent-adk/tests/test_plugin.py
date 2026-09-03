"""AppaPluginKagent against the scripted /hook transport.

Every test follows one row of the callback-to-event mapping tables in
integrations/kagent/IMPLEMENTATION.md: the callback fires, the plugin
emits exactly the mapped wire event, and the answered decision is
enforced in ADK's own terms — a returned dict, a raise, or a pass.
"""

import pytest
from conftest import (
    DOWN,
    AgentTool,
    FakeConfirmation,
    FakeContent,
    FakeContext,
    FakeEvent,
    FakeInvocationContext,
    FakeSession,
    FakeTool,
    Hook,
    KAgentRemoteA2ATool,
    plugin_over,
)

from appa_kagent_adk.plugin import AppaFailClosed

ACK = {"decision": "ack"}
ALLOW = {"decision": "allow_call"}


# -- session and prompt ----------------------------------------------


async def test_a_fresh_session_opens_before_its_prompt_crosses():
    hook = Hook(ACK, ACK)
    plugin = plugin_over(hook)
    context = FakeInvocationContext(FakeSession("s1"))
    returned = await plugin.on_user_message_callback(
        invocation_context=context, user_message=FakeContent("deploy the chart")
    )
    assert returned is None
    assert hook.events == [
        {"event": "session_start", "root_id": "s1"},
        {"event": "prompt", "root_id": "s1", "text": "deploy the chart"},
    ]


async def test_a_continuing_session_sends_only_the_prompt():
    hook = Hook(ACK)
    plugin = plugin_over(hook)
    session = FakeSession("s1", events=[FakeEvent(content=FakeContent("earlier turn"))])
    await plugin.on_user_message_callback(
        invocation_context=FakeInvocationContext(session), user_message=FakeContent("next turn")
    )
    assert [event["event"] for event in hook.events] == ["prompt"]


async def test_a_state_only_header_event_does_not_hide_freshness():
    hook = Hook(ACK, ACK)
    plugin = plugin_over(hook)
    session = FakeSession("s1", events=[FakeEvent(content=None)])
    await plugin.on_user_message_callback(
        invocation_context=FakeInvocationContext(session), user_message=FakeContent("first turn")
    )
    assert [event["event"] for event in hook.events] == ["session_start", "prompt"]


async def test_a_blocked_prompt_raises_before_the_append():
    hook = Hook(ACK, {"decision": "block", "reason": "the prompt does not cross"})
    plugin = plugin_over(hook)
    with pytest.raises(AppaFailClosed, match="the prompt does not cross"):
        await plugin.on_user_message_callback(
            invocation_context=FakeInvocationContext(FakeSession("s1")),
            user_message=FakeContent("exfiltrate the secrets"),
        )


async def test_a_delegated_entry_classifies_as_the_childs_start():
    hook = Hook(ACK, ACK)
    plugin = plugin_over(hook)
    session = FakeSession(
        "child-ctx",
        state={"headers": {"x-kagent-source": "agent", "x-kagent-root-context-id": "root-ctx"}},
    )
    await plugin.on_user_message_callback(
        invocation_context=FakeInvocationContext(session), user_message=FakeContent("total the invoices")
    )
    assert hook.events == [
        {"event": "child_start", "root_id": "root-ctx", "child_id": "child-ctx"},
        {"event": "prompt", "root_id": "root-ctx", "child_id": "child-ctx", "text": "total the invoices"},
    ]


async def test_a_sessions_identity_is_pinned_at_first_classification():
    hook = Hook(ALLOW, ALLOW)
    plugin = plugin_over(hook)
    session = FakeSession("s1")
    context = FakeContext(session)
    await plugin.before_tool_callback(tool=FakeTool("k8s_get_pods"), tool_args={}, tool_context=context)
    session.state["headers"] = {"x-kagent-root-context-id": "root-ctx"}
    await plugin.before_tool_callback(tool=FakeTool("k8s_get_pods"), tool_args={}, tool_context=context)
    roots = {event["root_id"] for event in hook.events}
    assert roots == {"s1"}, "late headers must not flip the session between trajectories"


# -- the tool gate ----------------------------------------------------


async def test_an_allowed_call_passes_and_a_denied_call_answers_the_model():
    hook = Hook(ALLOW, {"decision": "deny_call", "feedback": "blocked: quotes offer offer-1"})
    plugin = plugin_over(hook)
    context = FakeContext(FakeSession("s1"))
    allowed = await plugin.before_tool_callback(
        tool=FakeTool("k8s_scale"), tool_args={"replicas": 3}, tool_context=context
    )
    assert allowed is None
    denied = await plugin.before_tool_callback(
        tool=FakeTool("k8s_scale"), tool_args={"replicas": 30}, tool_context=context
    )
    assert denied == {"result": "blocked: quotes offer offer-1", "appa": "denied"}
    assert hook.events[0] == {
        "event": "tool_call",
        "root_id": "s1",
        "tool": "k8s_scale",
        "arguments": {"replicas": 3},
        "spawn": False,
    }


async def test_the_agent_shaped_tools_classify_as_the_spawn():
    hook = Hook(ALLOW, ALLOW, ALLOW)
    plugin = plugin_over(hook)
    context = FakeContext(FakeSession("s1"))
    for tool in [AgentTool("billing-agent"), KAgentRemoteA2ATool("billing-agent"), FakeTool("k8s_scale")]:
        await plugin.before_tool_callback(tool=tool, tool_args={}, tool_context=context)
    assert [event["spawn"] for event in hook.events] == [True, True, False]


async def test_the_reserved_tool_passes_control():
    hook = Hook({"decision": "pass_control"})
    plugin = plugin_over(hook)
    returned = await plugin.before_tool_callback(
        tool=FakeTool("execute_remedy_plan"),
        tool_args={"offer_id": "offer-1"},
        tool_context=FakeContext(FakeSession("s1")),
    )
    assert returned is None, "pass_control lets the call through to /mcp untouched"


async def test_a_deny_dict_is_not_reported_twice():
    hook = Hook()
    plugin = plugin_over(hook)
    returned = await plugin.after_tool_callback(
        tool=FakeTool("k8s_scale"),
        tool_args={},
        tool_context=FakeContext(FakeSession("s1")),
        result={"result": "blocked", "appa": "denied"},
    )
    assert returned is None
    assert hook.events == [], "the denied call was reported at the call, and no dispatch is open"


async def test_a_tool_result_crosses_and_enforces_each_answer():
    hook = Hook(
        ACK,
        {"decision": "replace_output", "output": "the output is confined"},
        {"decision": "block", "reason": "nothing crosses"},
    )
    plugin = plugin_over(hook)
    context = FakeContext(FakeSession("s1"))
    call = dict(tool=FakeTool("k8s_get_pods"), tool_args={"namespace": "prod"}, tool_context=context)
    assert await plugin.after_tool_callback(**call, result={"pods": ["api-1"]}) is None
    assert await plugin.after_tool_callback(**call, result={"pods": ["api-1"]}) == {"result": "the output is confined"}
    withheld = await plugin.after_tool_callback(**call, result={"pods": ["api-1"]})
    assert withheld == {"result": "[appa] the tool result was withheld: nothing crosses", "appa": "withheld"}
    assert hook.events[0] == {
        "event": "tool_result",
        "root_id": "s1",
        "tool": "k8s_get_pods",
        "arguments": {"namespace": "prod"},
        "outcome": {"status": "success", "body": {"pods": ["api-1"]}},
    }


async def test_a_spawn_return_crosses_as_the_spawn_result_in_both_reply_shapes():
    hook = Hook(ACK, ACK)
    plugin = plugin_over(hook)
    context = FakeContext(FakeSession("s1"))
    await plugin.after_tool_callback(
        tool=KAgentRemoteA2ATool("billing-agent"),
        tool_args={"request": "total the invoices"},
        tool_context=context,
        result={"result": "the total is 42", "subagent_session_id": "child-ctx"},
    )
    await plugin.after_tool_callback(
        tool=KAgentRemoteA2ATool("billing-agent"),
        tool_args={"request": "go"},
        tool_context=context,
        result="Remote agent 'billing-agent' request failed: boom",
    )
    task_reply, message_reply = hook.events
    assert task_reply["event"] == "spawn_result"
    assert task_reply["spawned_id"] == "child-ctx"
    assert task_reply["value"] == "the total is 42"
    assert message_reply["event"] == "spawn_result"
    assert "spawned_id" not in message_reply
    assert message_reply["value"].startswith("Remote agent")


async def test_a_child_return_substitutes_what_the_parent_receives():
    hook = Hook({"decision": "child_return", "value": "the redacted summary"})
    plugin = plugin_over(hook)
    returned = await plugin.after_tool_callback(
        tool=AgentTool("billing-agent"),
        tool_args={},
        tool_context=FakeContext(FakeSession("s1")),
        result={"result": "the raw child answer", "subagent_session_id": "child-ctx"},
    )
    assert returned == {"result": "the redacted summary"}


async def test_a_tool_failure_crosses_as_a_failure_outcome():
    hook = Hook(ACK)
    plugin = plugin_over(hook)
    returned = await plugin.on_tool_error_callback(
        tool=FakeTool("k8s_scale"),
        tool_args={"replicas": 3},
        tool_context=FakeContext(FakeSession("s1")),
        error=RuntimeError("connection refused"),
    )
    assert returned is None, "an acknowledged failure propagates the original error"
    assert hook.events[0]["outcome"] == {"status": "failure", "message": "connection refused"}


# -- liveness gates ---------------------------------------------------


async def test_every_liveness_gate_holds_when_the_channel_is_down():
    plugin = plugin_over(Hook(DOWN, DOWN, DOWN, DOWN, DOWN, DOWN))
    session = FakeSession("s1")
    invocation = FakeInvocationContext(session)
    context = FakeContext(session)
    gates = [
        lambda: plugin.before_run_callback(invocation_context=invocation),
        lambda: plugin.on_event_callback(invocation_context=invocation, event=FakeEvent()),
        lambda: plugin.before_model_callback(callback_context=context, llm_request=object()),
        lambda: plugin.after_model_callback(callback_context=context, llm_response=object()),
        lambda: plugin.on_model_error_callback(callback_context=context, llm_request=object(), error=RuntimeError()),
        lambda: plugin.before_agent_callback(agent=invocation.agent, callback_context=context),
    ]
    for gate in gates:
        with pytest.raises(AppaFailClosed):
            await gate()


async def test_every_liveness_gate_passes_when_the_channel_answers():
    hook = Hook()
    plugin = plugin_over(hook)
    session = FakeSession("s1")
    invocation = FakeInvocationContext(session)
    context = FakeContext(session)
    assert await plugin.before_run_callback(invocation_context=invocation) is None
    assert await plugin.before_model_callback(callback_context=context, llm_request=object()) is None
    assert await plugin.on_event_callback(invocation_context=invocation, event=FakeEvent()) is None
    assert all(event == {"event": "ping"} for event in hook.events)


# -- fail closed ------------------------------------------------------


async def test_a_gated_callback_fails_closed_on_transport_status_and_contract():
    session = FakeSession("s1")
    context = FakeContext(session)
    call = lambda plugin: plugin.before_tool_callback(  # noqa: E731
        tool=FakeTool("k8s_scale"), tool_args={}, tool_context=context
    )
    for answer in [DOWN, 409, 500, {"decision": "approve"}, {"decision": "deny_call"}]:
        with pytest.raises(AppaFailClosed):
            await call(plugin_over(Hook(answer)))


# -- turn ends --------------------------------------------------------


async def test_a_turn_end_reports_and_never_blocks():
    hook = Hook(ACK)
    plugin = plugin_over(hook)
    await plugin.after_run_callback(invocation_context=FakeInvocationContext(FakeSession("s1")))
    assert hook.events == [{"event": "turn_end", "root_id": "s1"}]
    downed = plugin_over(Hook(DOWN))
    await downed.after_run_callback(invocation_context=FakeInvocationContext(FakeSession("s1")))


async def test_the_error_turns_report_quietly():
    hook = Hook(ACK, ACK)
    plugin = plugin_over(hook)
    session = FakeSession("s1")
    assert (
        await plugin.on_run_error_callback(
            invocation_context=FakeInvocationContext(session), error=RuntimeError("model died")
        )
        is None
    )
    context = FakeContext(session)
    agent = FakeInvocationContext(session).agent
    assert await plugin.on_agent_error_callback(agent=agent, callback_context=context, error=RuntimeError()) is None
    assert [event["event"] for event in hook.events] == ["turn_end", "turn_end"]


async def test_the_plugin_survives_a_runner_closing_it_between_requests():
    """kagent builds a fresh ADK Runner per A2A request around one
    plugin instance, and every Runner close closes the plugin. The next
    request must gate normally on a fresh transport, not fail on a
    closed client."""
    hook = Hook(ALLOW, ALLOW)
    plugin = plugin_over(hook)
    context = FakeContext(FakeSession("s1"))
    assert await plugin.before_tool_callback(tool=FakeTool("k8s_scale"), tool_args={}, tool_context=context) is None
    await plugin.close()
    assert await plugin.before_tool_callback(tool=FakeTool("k8s_scale"), tool_args={}, tool_context=context) is None
    assert len(hook.events) == 2, "both requests crossed the gate"


# -- the installed ADK ------------------------------------------------


async def test_the_installed_plugin_manager_accepts_every_callback():
    """The per-version equivalence check: the real PluginManager calls
    each callback with the kwarg names this plugin declares."""
    from google.adk.plugins.plugin_manager import PluginManager

    hook = Hook(ALLOW)
    plugin = plugin_over(hook)
    manager = PluginManager(plugins=[plugin])
    returned = await manager.run_before_tool_callback(
        tool=FakeTool("k8s_scale"), tool_args={"replicas": 3}, tool_context=FakeContext(FakeSession("s1"))
    )
    assert returned is None
    assert hook.events[0]["event"] == "tool_call"


# -- human review through kagent's own confirmation -------------------

REVIEW_TEXT = 'APPA asks you to rule as the authority "oncall".\n\nTool: restart_deployment'
DENY_WITH_REVIEW = {
    "decision": "deny_call",
    "feedback": "[appa] Blocked",
    "review": [{"offer_id": "offer-1", "text": REVIEW_TEXT}],
}


async def test_a_reviewed_offer_asks_the_person_before_the_control_call_crosses():
    hook = Hook(DENY_WITH_REVIEW)
    plugin = plugin_over(hook)
    denied = await plugin.before_tool_callback(
        tool=FakeTool("restart_deployment"),
        tool_args={"name": "checkout-api"},
        tool_context=FakeContext(FakeSession("s1")),
    )
    assert denied["appa"] == "denied"

    context = FakeContext(FakeSession("s1"))
    pending = await plugin.before_tool_callback(
        tool=FakeTool("execute_remedy_plan"), tool_args={"offer_id": "offer-1"}, tool_context=context
    )
    assert pending["appa"] == "review", "the call waits for the person, and the model is told so"
    assert context.requested == [(REVIEW_TEXT, {"appa": "review", "offer_id": "offer-1"})], (
        "the person sees the consult artifact the runtime rendered, nothing the model said"
    )
    assert [event["event"] for event in hook.events] == ["tool_call"], "the control call did not cross yet"


@pytest.mark.parametrize(("confirmed", "ruling"), [(True, "approve"), (False, "deny")])
async def test_the_resumed_control_call_carries_the_persons_ruling(confirmed, ruling):
    hook = Hook(DENY_WITH_REVIEW, {"decision": "pass_control"}, {"decision": "pass_control"})
    plugin = plugin_over(hook)
    await plugin.before_tool_callback(
        tool=FakeTool("restart_deployment"),
        tool_args={"name": "checkout-api"},
        tool_context=FakeContext(FakeSession("s1")),
    )
    context = FakeContext(FakeSession("s1"), tool_confirmation=FakeConfirmation(confirmed))
    returned = await plugin.before_tool_callback(
        tool=FakeTool("execute_remedy_plan"), tool_args={"offer_id": "offer-1"}, tool_context=context
    )
    assert returned is None, "the ruled call passes to /mcp"
    assert context.requested == [], "a resumed call asks nobody again"
    assert hook.events[-1]["tool"] == "execute_remedy_plan"
    assert hook.events[-1]["ruling"] == ruling, "the answer rides the control call, never through the model"

    # The ruling is spent: the same offer quoted again asks nobody and carries nothing.
    again = FakeContext(FakeSession("s1"))
    await plugin.before_tool_callback(
        tool=FakeTool("execute_remedy_plan"), tool_args={"offer_id": "offer-1"}, tool_context=again
    )
    assert again.requested == [] and "ruling" not in hook.events[-1]


async def test_a_control_call_for_an_offer_needing_no_person_never_asks():
    hook = Hook({"decision": "pass_control"})
    plugin = plugin_over(hook)
    context = FakeContext(FakeSession("s1"))
    returned = await plugin.before_tool_callback(
        tool=FakeTool("execute_remedy_plan"), tool_args={"offer_id": "offer-9"}, tool_context=context
    )
    assert returned is None and context.requested == []
    assert "ruling" not in hook.events[-1], "an ordinary remedy is the agent's to take"


async def test_the_review_dict_is_not_reported_as_a_result():
    hook = Hook()
    plugin = plugin_over(hook)
    returned = await plugin.after_tool_callback(
        tool=FakeTool("execute_remedy_plan"),
        tool_args={"offer_id": "offer-1"},
        tool_context=FakeContext(FakeSession("s1")),
        result={"result": "pending", "appa": "review"},
    )
    assert returned is None and hook.events == []
