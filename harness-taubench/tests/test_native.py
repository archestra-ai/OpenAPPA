from tau2.environment.tool import as_tool

from appa_taubench.native import Allowed, Blocked, FrameworkSession

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
