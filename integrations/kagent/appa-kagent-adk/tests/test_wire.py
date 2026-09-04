"""The wire module against the shared fixtures and the decision contract.

The Go plugin's tests read the same fixture file, so the two plugins
cannot drift apart silently: an event this side builds is the event
that side builds, and both are the canonical envelope the runtime
parses (``appa-runtime-api/src/wire.rs``).
"""

import json
from pathlib import Path

import pytest

from appa_kagent_adk import wire

FIXTURES = Path(__file__).parent.parent.parent / "fixtures" / "wire-events.jsonl"

ROOT = "adk-4f6c2f1e"
CHILD = "adk-9b0d11aa"


def fixture_wires() -> dict[str, dict]:
    lines = [json.loads(line) for line in FIXTURES.read_text().splitlines() if line.strip()]
    return {fixture["name"]: fixture["wire"] for fixture in lines}


def test_every_builder_spells_its_fixture_exactly():
    fixtures = fixture_wires()
    scale = {"deployment": "api", "replicas": 3}
    built = {
        "session_start": wire.session_start(ROOT),
        "prompt": wire.prompt(ROOT, "scale the api deployment to three replicas"),
        "turn_end_root": wire.turn_end(ROOT),
        "turn_end_child": wire.turn_end(ROOT, child_id=CHILD),
        "tool_call_mcp": wire.tool_call(ROOT, "mcp:demo-tools/k8s_scale", scale),
        "tool_call_agent": wire.tool_call(ROOT, "agent:kagent/billing-agent", {"message": "total the invoices"}),
        "tool_call_builtin": wire.tool_call(
            ROOT, "builtin:ask_user", {"questions": [{"question": "which namespace?"}]}
        ),
        "tool_call_gate": wire.tool_call(ROOT, "gate:code_execution", {"code": "print(1)"}),
        "tool_call_control": wire.tool_call(ROOT, wire.CONTROL_TOOL, {"offer_id": "offer-1"}),
        "tool_call_control_ruled": wire.tool_call(ROOT, wire.CONTROL_TOOL, {"offer_id": "offer-1"}, ruling="approve"),
        "tool_result_success": wire.tool_result(
            ROOT, "mcp:demo-tools/k8s_scale", scale, wire.success({"scaled": True})
        ),
        "tool_result_failure": wire.tool_result(
            ROOT, "mcp:demo-tools/k8s_scale", scale, wire.failure("connection refused")
        ),
        "tool_result_null_body": wire.tool_result(ROOT, "mcp:demo-tools/k8s_scale", scale, wire.success(None)),
        "tool_result_without_body": wire.tool_result(
            ROOT, "mcp:demo-tools/k8s_scale", scale, wire.success_without_body()
        ),
        "spawn_result": wire.spawn_result(
            ROOT,
            "agent:kagent/billing-agent",
            {"message": "total the invoices"},
            wire.success({"result": "the total is 42"}),
            spawned_id=CHILD,
            value="the total is 42",
        ),
        "child_start_in_flight": wire.child_start(ROOT, CHILD),
        "child_start_bound": wire.child_start(ROOT, CHILD, spawn_binding="spawn-1"),
        "child_end_returned": wire.child_end(ROOT, CHILD, value="the total is 42"),
        "child_end_void": wire.child_end(ROOT, CHILD),
        "ping": wire.ping(),
    }
    assert set(built) == set(fixtures), "the fixture file and this test cover the same wire kinds"
    for name, expected in fixtures.items():
        assert built[name] == expected, f"the {name} wire drifted from the shared fixture"


def test_every_event_carries_the_envelope_and_no_spawn_flag():
    for event in fixture_wires().values():
        assert (event["protocol"], event["adapter"]) == (wire.PROTOCOL, wire.ADAPTER)
        assert "spawn" not in event


def test_a_success_says_whether_it_carries_a_body():
    """Three answers, three encodings: a body that is JSON ``null``, a
    body the wire does not carry, and an ordinary body."""
    assert wire.success(None) == {"status": "success", "body": None}
    assert wire.success_without_body() == {"status": "success_without_body"}
    assert "body" not in wire.success_without_body()
    assert wire.success({"scaled": True}) == {"status": "success", "body": {"scaled": True}}


def test_ids_cross_unprefixed():
    event = wire.tool_call("session-1", "mcp:demo-tools/k8s_scale", {}, child_id="child-1")
    assert (event["root_id"], event["child_id"]) == ("session-1", "child-1")


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
    assert wire.parse_decision(json.dumps({"protocol": 1, **body})) == expected, f"the {name} envelope drifted"


@pytest.mark.parametrize(
    ("name", "body"),
    [
        ("not_json", b"not json"),
        ("not_object", b'["decision"]'),
        ("no_protocol", b'{"decision": "ack"}'),
        ("other_protocol", b'{"protocol": 2, "decision": "ack"}'),
        ("protocol_as_bool", b'{"protocol": true, "decision": "ack"}'),
        ("protocol_as_float", b'{"protocol": 1.0, "decision": "ack"}'),
        ("protocol_as_exponent", b'{"protocol": 1e0, "decision": "ack"}'),
        ("protocol_as_string", b'{"protocol": "1", "decision": "ack"}'),
        ("unknown_kind", b'{"protocol": 1, "decision": "approve"}'),
        ("deny_without_feedback", b'{"protocol": 1, "decision": "deny_call"}'),
        ("block_without_reason", b'{"protocol": 1, "decision": "block"}'),
        ("replace_without_output", b'{"protocol": 1, "decision": "replace_output"}'),
        ("refuse_without_detail", b'{"protocol": 1, "decision": "refuse"}'),
        ("context_without_text", b'{"protocol": 1, "decision": "context"}'),
        ("offers_not_a_list", b'{"protocol": 1, "decision": "deny_call", "feedback": "f", "offers": {}}'),
        (
            "offer_without_an_id",
            b'{"protocol": 1, "decision": "deny_call", "feedback": "f", "offers": [{"returns": "as_spoken"}]}',
        ),
        (
            "offer_route_outside_the_wire",
            b'{"protocol": 1, "decision": "deny_call", "feedback": "f",'
            b' "offers": [{"offer_id": "o1", "returns": "shaped"}]}',
        ),
        (
            "review_not_a_list",
            b'{"protocol": 1, "decision": "deny_call", "feedback": "f", "review": "o1"}',
        ),
        (
            "review_entry_without_text",
            b'{"protocol": 1, "decision": "deny_call", "feedback": "f", "review": [{"offer_id": "o1"}]}',
        ),
        (
            "sanitized_route_without_a_sanitizer",
            b'{"protocol": 1, "decision": "deny_call", "feedback": "f", "offers": [{"offer_id": "o1", "returns": {}}]}',
        ),
        ("binding_not_a_string", b'{"protocol": 1, "decision": "allow_call", "spawn_binding": 7}'),
    ],
)
def test_an_envelope_outside_the_contract_is_a_wire_error(name, body):
    with pytest.raises(wire.WireError):
        wire.parse_decision(body)


def test_a_deny_without_offers_or_review_carries_none():
    decision = wire.parse_decision(b'{"protocol": 1, "decision": "deny_call", "feedback": "f"}')
    assert (decision.offers, decision.review) == ((), ())


def test_a_control_call_carries_its_ruling_only_when_given():
    ruled = wire.tool_call("s1", wire.CONTROL_TOOL, {"offer_id": "o1"}, ruling="approve")
    assert ruled["ruling"] == "approve"
    assert "ruling" not in wire.tool_call("s1", wire.CONTROL_TOOL, {"offer_id": "o1"})


def test_a_child_end_without_a_value_keeps_it_off_the_wire():
    assert "value" not in wire.child_end("s1", "c1")
    assert wire.child_end("s1", "c1", value="the total is 42")["value"] == "the total is 42"


def test_a_deny_carries_its_offers_and_review():
    body = {
        "protocol": 1,
        "decision": "deny_call",
        "feedback": "[appa] Blocked",
        "offers": [
            {"offer_id": "offer-1", "returns": "as_spoken"},
            {"offer_id": "offer-2", "returns": {"sanitizer": "strip-instructions"}},
            {"offer_id": "offer-3"},
        ],
        "review": [{"offer_id": "offer-3", "text": "rule on this"}],
    }
    decision = wire.parse_decision(json.dumps(body))
    assert decision.offers == (
        wire.Offer("offer-1", returns=wire.RETURN_AS_SPOKEN),
        wire.Offer("offer-2", returns=wire.RETURN_SANITIZED, sanitizer="strip-instructions"),
        wire.Offer("offer-3"),
    )
    assert decision.review == (("offer-3", "rule on this"),)
