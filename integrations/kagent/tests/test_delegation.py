"""Delegation: the child's own stop, the parent's replay, and the fork that binds.

Five conversations, each deterministic. Where a return is judged is what
they pin. The child's value crosses at its own stop, and the parent's
spawn result replays what crossed: the parent's gate declares nothing
new and adds nothing of its own. So a clean crossing is what a released
delegation ends in, and the parent's model reads the child's words as an
ordinary tool result.

The parent's model never sees the return declaration. A marked spawn is
blocked with the menu of return routes, and the plugin routes the bare
floor itself — the parent's own current label — then re-proposes the
call, so the model reads one tool call and its result. Every case
asserts that: the parent never calls the reserved tool.

The floor the parent declares then binds the child's whole branch. A
narrowing that would take the child below it is refused in the child's
own branch, which is why the analysis case takes the sanitizer and never
the change.

The assertions read the parent's task, because that is all a delegation
shows its caller. The child's own conversation is a separate A2A task on
the child's port, so what the child's model read at its stop comes from
the harness (`stack.child_read`, `stack.child_saw`).
"""

import json
import urllib.parse
import urllib.request
import uuid

import pytest

from conftest import CHILD_FAILURE, CHILD_TOOL, INJECTION, UNDECLARED_TOOL

# The parent's prompts, and the `request` each delegation carries. The
# child's script is keyed by that request, so the two must agree.
GREET = "introduce yourself in one sentence"
BRIEF = "ask the log analyst to introduce itself"
ANALYZE = "analyze the crash logs of checkout-api-b2k1 and report the errors you find"
DELEGATE = f"ask the log analyst to {ANALYZE}"
QUIET = "read the upstream status page and return nothing at all"
DELEGATE_QUIET = f"ask the log analyst to {QUIET}"
BUMP = "approve a version bump of checkout-api to 2.4.1"
DELEGATE_UNDECLARED = f"ask the release manager to {BUMP}"

# What the child says at its stop. Each case asserts on the exact bytes,
# because a replay at the parent is byte-for-byte: the runtime matches
# the message the parent delivers against the value that crossed, and
# withholds anything else.
ANALYST = "I am the log analyst, and I read pod logs."
SUMMARY = "checkout-api-b2k1 restarted 14 times and its log ends in an OOMKilled error."
LATE = "one more thing about the status page"

# The reserved tool. A delegation never puts it in the parent's
# transcript: the plugin routes the return declaration through the
# runtime itself (`appa_kagent_adk/plugin.py`).
RESERVED = "execute_remedy_plan"

# The runtime's own words, quoted where a case turns on which one came
# back. `NOT_DECLARED` is the denial at an unnamed spawn
# (`appa-runtime/src/api/mod.rs`, `UndeclaredSpawn`). `SPAWN_NOT_TAKEN`
# is the refusal that means the runtime tied no child to this parent's
# prepared fork, so nothing crossed and the delegation did not happen.
NOT_DECLARED = "not declared by the policy"
SPAWN_NOT_TAKEN = "the spawn did not take"

# The APPA-owned tool a child scope stops through, and the plugin's own
# answer to a stop that crossed nothing (`appa_kagent_adk/plugin.py`).
# The child reads that answer as an ordinary tool result and ends with
# an empty final message.
RETURN_TOOL = "appa_return"
VOID_CROSSED = "the void return crossed"

# The two offers a narrowing can settle with, as the feedback renders
# them (`appa-runtime/src/engine.rs`, `remedy_instruction`). Which of
# them the child is offered is the whole point of the second case.
ACCEPT = "Accept this change"
SANITIZE = "Use sanitizer strip-instructions"


def status(runtime_url: str, context_id: str) -> dict:
    """The runtime's reading of one root trajectory's current label.

    The runtime names a kagent root trajectory `kagent:<context id>`, and
    an A2A task carries its context id. The read is a projection: it
    gates nothing and changes nothing.
    """
    query = urllib.parse.urlencode({"trajectory": f"kagent:{context_id}"})
    with urllib.request.urlopen(f"{runtime_url}/status?{query}", timeout=10) as answer:
        return json.load(answer)


def crossing(task, tool: str = CHILD_TOOL) -> dict:
    """The one response a delegation gave the parent's model.

    Asserts the shared floor of every released delegation on the way:
    the call carried arguments, it answered, it was not denied, and a
    child bound this parent's prepared fork.
    """
    calls = task.calls(tool)
    assert calls, "the parent proposed the delegation"
    responses = task.responses(tool)
    assert responses, "the delegation answered the parent's model"
    returned = responses[0]
    assert returned.get("appa") != "denied", f"the policy names this child, so the spawn is released: {returned}"
    body = json.dumps(returned, default=str)
    assert SPAWN_NOT_TAKEN not in body, f"a child bound this parent's prepared fork: {returned}"
    assert not CHILD_FAILURE.search(body), f"the child answered, and kagent's own no-answer text did not: {returned}"
    assert not task.calls(RESERVED), (
        "the plugin routed the return declaration, so the model never called the reserved tool"
    )
    return returned


# ------------------------------------------------- one added fixture
#
# The harness fixtures serve every case here except the last one, which
# needs two parents to meet in one child session. kagent keys a child
# session by the caller's user id and the context id the caller sends,
# and its python remote-agent tool draws a fresh context id in every
# constructor while kagent derives the user id from the calling session
# (`A2A_USER_<context id>`). Two python parents therefore reach the child
# in two sessions. kagent's go tool sends every delegation of one pod
# into one context id, which is the shape the (root, child) pair exists
# for. The fixture pins both halves of the key, so the shared child
# session is deterministic on the lane this suite drives.


@pytest.fixture
def one_child_session(monkeypatch) -> str:
    """Send every delegation of this case into one child session."""
    from kagent.adk import _remote_a2a_tool
    from kagent.adk.converters import request_converter

    shared = f"shared-child-context-{uuid.uuid4()}"
    stock_init = _remote_a2a_tool.KAgentRemoteA2ATool.__init__

    def pinned_init(self, **arguments):
        stock_init(self, **arguments)
        self._last_context_id = shared

    monkeypatch.setattr(_remote_a2a_tool.KAgentRemoteA2ATool, "__init__", pinned_init)
    monkeypatch.setattr(request_converter, "_get_user_id", lambda request: "one-operator")
    return shared


# ------------------------------------------------------------ the cases


def test_the_child_s_value_crosses_at_its_own_stop_and_the_parent_replays_it(stack, runtime_url):
    """The declared delegation with nothing to sanitize, end to end.

    The policy names `agent/kagent/log-analyst`, so the spawn is blocked
    with the return menu, the plugin declares the bare floor, and the
    re-proposed call runs. The child's entry binds the prepared fork.

    The child reads nothing, so its branch stands where the fork seeded
    it: at the parent's own label, the floor. Its one sentence crosses at
    its stop as spoken. The parent's spawn result then carries that same
    message, the runtime matches it against what crossed, and replays it
    — so the parent's model reads the child's sentence, byte for byte,
    as an ordinary tool result. Nothing is withheld and nothing is
    substituted, because the value was already checked where the child
    said it.
    """
    stack.script_child(GREET, [{"text": ANALYST}])
    task = stack.say(
        BRIEF,
        [
            {"tool": CHILD_TOOL, "args": {"request": GREET}},
            {"text": "The analyst answered."},
        ],
    )

    assert task.state == "completed"
    assert task.calls(CHILD_TOOL)[0]["args"].get("request") == GREET, "the request the child scripts for"
    returned = crossing(task)
    assert returned.get("appa") is None, f"the child's value crossed, so nothing is gated at the parent: {returned}"
    assert returned.get("result") == ANALYST, f"the parent replays exactly what crossed at the child's stop: {returned}"
    assert isinstance(returned.get("subagent_session_id"), str), f"the child's own session came back: {returned}"
    assert task.confirmation() is None, "no person is asked about a delegation"
    assert status(runtime_url, task.context_id)["trust"] == "trusted", (
        "a return at the parent's own floor leaves the parent's label where it was"
    )


def test_the_floor_the_parent_declared_binds_the_child_s_own_reads(stack, runtime_url):
    """The child works on the hazard, inside the floor it was given.

    The parent's declaration is the bare floor: the child's return must
    stand at the parent's own label. That floor binds every narrowing
    the child proposes, not only its return. So when the child reads
    crash logs, which enter suspicious, the block in the child's own
    branch offers the sanitizer and does not offer the change: accepting
    it would drop the child below the floor, and the runtime does not
    offer a remedy whose result could never cross.

    The child takes the sanitizer, reads the derivation instead of the
    raw logs, and the instruction inside them reaches nothing. Its
    branch never left the floor, so its summary crosses as spoken and
    the parent replays it.
    """
    stack.script_child(
        ANALYZE,
        [
            {"tool": "get_pod_logs", "args": {"name": "checkout-api-b2k1"}},
            {"remedy": SANITIZE},
            {"tool": "get_pod_logs", "args": {"name": "checkout-api-b2k1"}},
            {"text": SUMMARY},
        ],
    )
    task = stack.say(
        DELEGATE,
        [
            {"tool": CHILD_TOOL, "args": {"request": ANALYZE}},
            {"text": "The analyst answered with its summary."},
        ],
    )

    assert task.state == "completed"
    reads = stack.child_results("get_pod_logs")
    assert len(reads) == 2, f"the child proposed the read, was denied, and proposed it again: {reads}"
    feedback = str(reads[0].get("result", ""))
    assert reads[0].get("appa") == "denied", f"the untrusted read is gated in the child's branch too: {reads[0]}"
    assert SANITIZE in feedback, f"the child is offered the sanitizer: {feedback}"
    assert ACCEPT not in feedback, (
        f"the floor the parent declared binds the child: a change it accepted could never cross: {feedback}"
    )
    derivation = json.dumps(reads[1], default=str)
    assert reads[1].get("appa") is None, f"the re-proposed read is not gated shut: {derivation}"
    assert INJECTION not in derivation, f"the derivation dropped the line addressed to the reader: {derivation}"

    returned = crossing(task)
    assert returned.get("appa") is None, f"the child's summary crossed at its stop: {returned}"
    assert returned.get("result") == SUMMARY, f"the parent replays exactly what crossed: {returned}"
    assert INJECTION not in task.everything(), "the instruction inside the logs never reaches the parent or the caller"
    assert not task.calls("get_pod_logs"), "the parent never read the logs itself"
    assert task.confirmation() is None, "no person is asked about a delegation"
    assert status(runtime_url, task.context_id)["trust"] == "trusted", (
        "the child's branch carried the ingress, and the parent's label did not move"
    )


def test_nothing_a_child_says_after_returning_nothing_reaches_its_parent(stack):
    """What a child says after returning nothing stays with the child.

    The child returns nothing: an empty final message is a void return,
    which crosses and ends its branch. The child stops through the
    APPA-owned tool, so the runtime judges that stop and its answer
    reaches the child as an ordinary tool result. The answer names the
    void crossing and tells the child to end with an empty message.

    The child speaks once more instead. The plugin holds what crossed
    and replays it at every later stop, so only one stop of this child
    reaches the runtime and nothing it says after the crossing can
    cross. The forced return gate is what makes that possible: a kagent
    child's final message would otherwise be its last word, and the
    runtime would judge a message the child was never told to stop with.

    The parent's spawn result carries no message: the child returned
    nothing, so the delegation answers the parent's model with an empty
    result and the child's session id, and nothing is withheld.
    """
    stack.script_child(
        QUIET,
        [
            {"text": ""},
            {"text": LATE},
        ],
    )
    task = stack.say(
        DELEGATE_QUIET,
        [
            {"tool": CHILD_TOOL, "args": {"request": QUIET}},
            {"text": "The analyst returned nothing."},
        ],
    )

    assert task.state == "completed"
    returned = crossing(task)
    assert LATE not in task.everything(), "nothing the child said after its void return crosses to the parent"
    assert INJECTION not in task.everything()
    assert returned.get("appa") is None, (
        f"the void crossed at the child's stop, so the parent's gate withheld nothing: {returned}"
    )
    assert not str(returned.get("result") or "").strip(), f"the parent reads the nothing the child returned: {returned}"
    stops = stack.child_results(RETURN_TOOL)
    assert len(stops) == 1, (
        f"one stop of this child reached the runtime, and the plugin replayed that crossing after it: {stops}"
    )
    assert VOID_CROSSED in str(stops[0].get("result", "")), (
        f"the runtime judged the child's own stop, and the child read the answer: {stops[0]}"
    )
    assert len(stack.child_turns()) > 1, (
        f"the child spoke again after its void return, and none of it crossed: {stack.child_turns()}"
    )
    assert task.confirmation() is None, "no person is asked about a delegation"


def test_a_delegation_the_policy_never_names_is_denied_at_the_spawn(stack):
    """The release manager is a tool the parent lists and no contract
    names. On kagent an agent runs as a child only under a contract that
    names it, and the wildcard covers no spawn. The runtime denies the
    call before it dispatches, so there is no return menu to route and
    no fork to bind.

    No child session opens. Both remote agents in this suite resolve to
    the child's port, and an entry there with no registered script
    answers with the harness's own line. That line never appears.
    """
    task = stack.say(
        DELEGATE_UNDECLARED,
        [
            {"tool": UNDECLARED_TOOL, "args": {"request": BUMP}},
            {"text": "The delegation was denied, so I did nothing."},
        ],
    )

    assert task.state == "completed"
    assert task.calls(UNDECLARED_TOOL), "the parent proposed the delegation"
    responses = task.responses(UNDECLARED_TOOL)
    assert responses, "the reserved answer reached the parent's model"
    denied = responses[0]
    assert denied.get("appa") == "denied", f"the delegation is denied, not run: {denied}"
    assert NOT_DECLARED in str(denied.get("result", "")), f"the runtime's own reason reaches the model: {denied}"
    assert "subagent_session_id" not in denied, f"no child session opened: {denied}"
    assert "[harness]" not in task.everything(), "the child app was never entered"
    assert not task.calls(RESERVED), "a denied spawn offers no return to declare"
    assert task.confirmation() is None, "a denied spawn asks nobody"


def test_two_parents_delegate_in_turn_into_one_child_session(stack, one_child_session):
    """Two parent sessions, one child session, both returns crossing.

    A child opens once per (root, child) pair, not once per session. The
    second parent enters the session the first left behind, so a plugin
    that opens a child by session freshness sends no `child_start` for
    it. The runtime then binds no fork for the second parent, refuses
    the child's events, and the second return never crosses. Both
    returns crossing is the regression this case holds.

    The case asserts the binding, not the child's work. The second entry
    replays the first entry's transcript, so the child's script is spent
    and the harness answers past its end. Both parents name the same
    child session, which is what one child serving two parents in turn
    looks like from the caller's side.
    """
    stack.script_child(GREET, [{"text": ANALYST}])
    turns = [
        {"tool": CHILD_TOOL, "args": {"request": GREET}},
        {"text": "The analyst answered."},
    ]
    first = stack.say(BRIEF, turns)
    second = stack.say(BRIEF, turns)

    assert first.context_id != second.context_id, "each parent ran in its own session"
    for parent, task in (("the first parent", first), ("the second parent", second)):
        assert task.state == "completed", parent
        returned = crossing(task)
        assert returned.get("appa") is None, f"the child's return crosses the gate of {parent}: {returned}"
        assert returned.get("result"), f"the child answered {parent} with something: {returned}"
        assert returned.get("subagent_session_id") == one_child_session, (
            f"one child session served {parent}: {returned}"
        )
    assert first.confirmation() is None and second.confirmation() is None, "nobody is asked"
