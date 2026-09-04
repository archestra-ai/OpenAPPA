"""The wire module against the shared fixtures and the decision contract.

The rust codec's tests parse the same fixture file, so the two language
halves cannot drift apart silently: an event this side builds is an
event that side parses.
"""

import json
from pathlib import Path

import pytest

from appa_kagent_adk import wire

FIXTURES = Path(__file__).parent.parent.parent / "fixtures" / "wire-events.jsonl"


def fixture_wires() -> dict[str, dict]:
    lines = [json.loads(line) for line in FIXTURES.read_text().splitlines() if line.strip()]
    return {fixture["name"]: fixture["wire"] for fixture in lines}


def test_every_builder_spells_its_fixture_exactly():
    fixtures = fixture_wires()
    built = {
        "session_start": wire.session_start("adk-4f6c2f1e"),
        "prompt": wire.prompt("adk-4f6c2f1e", "scale the api deployment to three replicas"),
        "turn_end_root": wire.turn_end("adk-4f6c2f1e"),
        "turn_end_child": wire.turn_end("adk-4f6c2f1e", child_id="adk-9b0d11aa"),
        "tool_call": wire.tool_call("adk-4f6c2f1e", "k8s_scale", {"deployment": "api", "replicas": 3}, spawn=False),
        "tool_call_spawn": wire.tool_call(
            "adk-4f6c2f1e", "billing-agent", {"message": "total the invoices"}, spawn=True
        ),
        "tool_call_reserved": wire.tool_call(
            "adk-4f6c2f1e", "execute_remedy_plan", {"offer_id": "offer-1"}, spawn=False
        ),
        "tool_call_reserved_ruled": wire.tool_call(
            "adk-4f6c2f1e", "execute_remedy_plan", {"offer_id": "offer-1"}, spawn=False, ruling="approve"
        ),
        "tool_result_success": wire.tool_result(
            "adk-4f6c2f1e",
            "k8s_scale",
            {"deployment": "api", "replicas": 3},
            wire.success({"scaled": True}),
        ),
        "tool_result_failure": wire.tool_result(
            "adk-4f6c2f1e",
            "k8s_scale",
            {"deployment": "api", "replicas": 3},
            wire.failure("connection refused"),
        ),
        "spawn_result": wire.spawn_result(
            "adk-4f6c2f1e",
            "billing-agent",
            {"message": "total the invoices"},
            wire.success({"result": "the total is 42"}),
            spawned_id="adk-9b0d11aa",
            value="the total is 42",
        ),
        "child_start_in_flight": wire.child_start("adk-4f6c2f1e", "adk-9b0d11aa"),
        "child_start_bound": wire.child_start("adk-4f6c2f1e", "adk-9b0d11aa", spawn_binding="spawn-1"),
        "child_end_returned": wire.child_end("adk-4f6c2f1e", "adk-9b0d11aa", value="the total is 42"),
        "child_end_void": wire.child_end("adk-4f6c2f1e", "adk-9b0d11aa"),
        "ping": wire.ping(),
    }
    assert set(built) == set(fixtures), "the fixture file and this test cover the same wire kinds"
    for name, expected in fixtures.items():
        assert built[name] == expected, f"the {name} wire drifted from the shared fixture"


def test_absent_optionals_stay_off_the_wire():
    event = wire.spawn_result("s1", "t", {}, wire.indeterminate())
    assert "spawned_id" not in event
    assert "value" not in event
    assert "child_id" not in event


@pytest.mark.parametrize(
    ("name", "body", "expected"),
    [
        ("ack", {"decision": "ack"}, wire.Decision(kind="ack")),
        ("allow", {"decision": "allow_call"}, wire.Decision(kind="allow_call")),
        (
            "allow_bound",
            {"decision": "allow_call", "spawn_binding": "b1"},
            wire.Decision(kind="allow_call", spawn_binding="b1"),
        ),
        ("pass", {"decision": "pass_control"}, wire.Decision(kind="pass_control")),
        (
            "deny",
            {"decision": "deny_call", "feedback": "blocked: the recipient cannot read this"},
            wire.Decision(kind="deny_call", feedback="blocked: the recipient cannot read this"),
        ),
        (
            "block",
            {"decision": "block", "reason": "the prompt does not cross"},
            wire.Decision(kind="block", reason="the prompt does not cross"),
        ),
        (
            "replace",
            {"decision": "replace_output", "output": "the output is confined"},
            wire.Decision(kind="replace_output", output="the output is confined"),
        ),
        (
            "child_return",
            {"decision": "child_return", "value": "the redacted summary"},
            wire.Decision(kind="child_return", value="the redacted summary"),
        ),
        (
            "context",
            {"decision": "context", "text": "your return crosses as you speak it"},
            wire.Decision(kind="context", text="your return crosses as you speak it"),
        ),
        (
            "refuse",
            {"decision": "refuse", "detail": "storage failure: disk full"},
            wire.Decision(kind="refuse", detail="storage failure: disk full"),
        ),
    ],
)
def test_every_decision_envelope_parses(name, body, expected):
    assert wire.parse_decision(json.dumps(body)) == expected, f"the {name} envelope drifted"


@pytest.mark.parametrize(
    ("name", "body"),
    [
        ("not_json", b"not json"),
        ("not_object", b'["decision"]'),
        ("unknown_kind", b'{"decision": "approve"}'),
        ("deny_without_feedback", b'{"decision": "deny_call"}'),
        ("block_without_reason", b'{"decision": "block"}'),
        ("replace_without_output", b'{"decision": "replace_output"}'),
        ("refuse_without_detail", b'{"decision": "refuse"}'),
        ("context_without_text", b'{"decision": "context"}'),
        ("offers_not_a_list", b'{"decision": "deny_call", "feedback": "f", "offers": {}}'),
        ("offer_without_an_id", b'{"decision": "deny_call", "feedback": "f", "offers": [{"returns": "as_spoken"}]}'),
        (
            "offer_route_outside_the_wire",
            b'{"decision": "deny_call", "feedback": "f", "offers": [{"offer_id": "o1", "returns": "shaped"}]}',
        ),
        (
            "sanitized_route_without_a_sanitizer",
            b'{"decision": "deny_call", "feedback": "f", "offers": [{"offer_id": "o1", "returns": {}}]}',
        ),
        ("binding_not_a_string", b'{"decision": "allow_call", "spawn_binding": 7}'),
    ],
)
def test_an_answer_outside_the_contract_is_a_wire_error(name, body):
    with pytest.raises(wire.WireError):
        wire.parse_decision(body)


def test_a_deny_carries_its_reviews_and_refuses_an_unreadable_one():
    from appa_kagent_adk.wire import WireError, parse_decision

    decision = parse_decision(
        b'{"decision":"deny_call","feedback":"f","review":[{"offer_id":"o1","text":"APPA asks you"}]}'
    )
    assert decision.review == (("o1", "APPA asks you"),)
    assert parse_decision(b'{"decision":"deny_call","feedback":"f"}').review == ()
    import pytest

    with pytest.raises(WireError):
        parse_decision(b'{"decision":"deny_call","feedback":"f","review":[{"offer_id":"o1"}]}')


def test_a_control_call_carries_its_ruling_only_when_given():
    from appa_kagent_adk.wire import tool_call

    ruled = tool_call("s1", "execute_remedy_plan", {"offer_id": "o1"}, False, ruling="approve")
    assert ruled["ruling"] == "approve"
    assert "ruling" not in tool_call("s1", "execute_remedy_plan", {"offer_id": "o1"}, False)


def test_a_deny_carries_every_offer_with_its_return_route():
    decision = wire.parse_decision(
        json.dumps(
            {
                "decision": "deny_call",
                "feedback": "blocked",
                "offers": [
                    {"offer_id": "o1"},
                    {"offer_id": "o2", "returns": "as_spoken"},
                    {"offer_id": "o3", "returns": {"sanitizer": "redact-invoices"}},
                ],
            }
        )
    )
    assert decision.offers == (
        wire.Offer(offer_id="o1"),
        wire.Offer(offer_id="o2", returns=wire.RETURN_AS_SPOKEN),
        wire.Offer(offer_id="o3", returns=wire.RETURN_SANITIZED, sanitizer="redact-invoices"),
    )
    assert wire.parse_decision(b'{"decision":"deny_call","feedback":"f"}').offers == ()


def test_a_child_end_without_a_value_keeps_it_off_the_wire():
    assert "value" not in wire.child_end("s1", "c1")
    assert wire.child_end("s1", "c1", value="the total is 42")["value"] == "the total is 42"
