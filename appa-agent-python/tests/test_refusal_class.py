import pytest
from appa_agent_python import AppaError, Session


def test_a_runtime_refusal_raises_with_its_class(session: Session) -> None:
    with pytest.raises(AppaError) as raised:
        session.check("never_declared", {})
    assert raised.value.refusal_class == "undeclared_tool"


def test_a_session_fault_raises_without_a_class(session: Session) -> None:
    with pytest.raises(AppaError) as raised:
        session.report("nothing was proposed")
    assert raised.value.refusal_class is None
