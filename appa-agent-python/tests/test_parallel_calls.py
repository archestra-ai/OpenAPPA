from appa_agent_python import Session

from conftest import decision


def test_identified_calls_report_out_of_order_and_abandon_independently(session: Session) -> None:
    for call_id, text in [("call-1", "one"), ("call-2", "two"), ("call-3", "three")]:
        allowed = decision(session.check("deliver_result", {"text": text}, call_id=call_id))
        assert allowed["kind"] == "allowed"

    second = decision(session.report("second", call_id="call-2"))
    first = decision(session.report("first", call_id="call-1"))
    assert second["kind"] == "delivered"
    assert first["kind"] == "delivered"

    session.abandon(call_id="call-3")
    session.close()
