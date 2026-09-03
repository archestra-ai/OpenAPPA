"""The spawn proposal itself, and the two ways a harness opens the child.

A harness that drives its own children spawns and opens in one call. A harness
that only observes them — one reading a subagent's hooks — answers the parent's
proposal first and opens the child from a later signal that names no spawn call.
"""

import json

import pytest
from conftest import PLAIN_POLICY, POLICY, RETURN_SCHEMA, TOOLS, accept_narrowing, declare_spawn, decision

from appa_agent_python import AppaError, Session


def test_a_spawn_the_policy_refuses_is_a_decision_and_not_an_error(session: Session):
    # The parent reads the ticket itself, so its own trust falls below what the
    # spawn tool requires.
    assert accept_narrowing(session, "read_ticket", {"id": "T-42"})["kind"] == "allowed"
    session.report("Grant 20 days.")

    answered, child = session.spawn_child("researcher_1", return_schema=RETURN_SCHEMA)
    assert decision(answered)["kind"] == "blocked"
    assert child is None

    with pytest.raises(AppaError, match="no child branch"):
        session.child("researcher_1")


def test_a_marked_spawn_is_held_until_the_parent_declares_its_return(session: Session):
    """The spawn blocks on the return menu; the declaration approves the exact
    call, which then releases with its fork."""
    refused = decision(session.check("delegate", {"task": "read the ticket"}, spawn=True))
    assert refused["kind"] == "blocked"

    released = declare_spawn(session, {"task": "read the ticket"})
    assert released["kind"] == "allowed"
    assert released["spawn_binding"], "a context-controlled spawn releases a fork"


def test_an_observing_harness_opens_the_child_from_the_spawn_in_flight(session: Session):
    """The child-start signal names no spawn call, so the runtime ties it to
    the family's one spawn in flight."""
    released = declare_spawn(session, {"task": "read the ticket"})
    assert released["kind"] == "allowed"

    child = session.open_child("researcher_1")
    assert child.child_id == "researcher_1"
    assert child.context is None, "a return crossing as spoken tells the child nothing"

    assert decision(child.finish("Grant 20 days."))["kind"] == "returned"


def test_an_observing_harness_may_name_the_binding_it_was_handed(session: Session):
    released = declare_spawn(session, {"task": "read the ticket"})
    child = session.open_child("researcher_1", binding=released["spawn_binding"])

    assert decision(child.finish("Grant 20 days."))["kind"] == "returned"


def test_an_unmarked_call_releases_no_fork(session: Session):
    released = decision(session.check("deliver_result", {"text": "hello"}))

    assert released["kind"] == "allowed"
    assert released["spawn_binding"] is None


def test_a_child_start_without_a_spawn_is_refused(session: Session):
    with pytest.raises(AppaError, match="no spawn call is pending"):
        session.open_child("researcher_1")


def test_the_schema_is_the_declaration_not_a_spawn_argument(session: Session):
    """The schema binds the return at the declaration; the spawn's own arguments
    reach the child untouched, and the child is told the shape at its start."""
    answered, child = session.spawn_child(
        "researcher_1",
        return_schema=RETURN_SCHEMA,
        arguments={"task": "read the ticket"},
    )

    opened = decision(answered)
    assert opened["kind"] == "opened"
    assert opened["dispatched_arguments"] == {"task": "read the ticket"}
    assert child.context, "an attested return tells the child its shape at the start"


def test_a_schema_outside_the_dialect_refuses_the_declaration(session: Session):
    with pytest.raises(AppaError, match="return_schema"):
        session.spawn_child(
            "researcher_1",
            return_schema={"type": "object", "properties": {"note": {"type": "string"}}, "required": ["note"]},
        )


def test_an_unshaped_child_stands_on_the_parents_floor(session: Session):
    """A return declared as spoken is floored at the parent's own label, and the
    floor bounds the child too: a read that would narrow it below the floor is
    offered no acceptance, and what the child does say crosses as it stands."""
    answered, child = session.spawn_child("researcher_1", arguments={"task": "read the ticket"})
    assert decision(answered)["kind"] == "opened"

    refused = decision(child.check("read_ticket", {"id": "T-42"}))
    assert refused["kind"] == "blocked"
    assert 'offer_id: "' not in refused["feedback"], refused["feedback"]

    assert decision(child.finish("The ticket is unread."))["kind"] == "returned"


def test_a_session_naming_no_spawn_tool_opens_no_children():
    session = Session(PLAIN_POLICY, json.dumps(TOOLS), "decide the ticket")

    with pytest.raises(AppaError, match="no spawn tool"):
        session.spawn_child("researcher_1", return_schema=RETURN_SCHEMA)


def test_a_return_sanitizer_needs_a_host_that_opens_children():
    """A host that names no spawn tool controls no child context, so nothing a
    child returns reaches the attesting sanitizer. The loader refuses rather than
    loading a sanitizer that could never run."""
    with pytest.raises(AppaError, match="attest-schema"):
        Session(POLICY, json.dumps(TOOLS), "decide the ticket")
