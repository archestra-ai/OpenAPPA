"""The remedy cases: every authority that rules on a blocked call, and every refusal.

Each case registers the turns its agent plays and asserts on substance.
Which authority ruled, which remedy ran, and what crossed the gate are
read from the task's function calls and their responses. The model's
wording is the script's own, so no assertion reads it.

Two cases steer the agent with a second user message in the same
context. The scripted model finds its script by the first user text of
the request, and the transcript carries the turn count, so one script
spans both messages. Its later turns are the reaction to the steer.

Every prompt below opens a different conversation. Two cases must never
open with the same words, because the opening words are the script key.
"""

import json
import re

from conftest import SECRET

STEER_ACCEPT = "read the payments-provider secret and report the block to me"
STEER_DECLINE = "read the payments-provider secret and hold for my instruction"
RESTART_APPROVED = "restart the checkout-api deployment for an approving reviewer"
RESTART_REJECTED = "restart the checkout-api deployment for a rejecting reviewer"
PUBLIC_RUNBOOK = "look up the public-oncall-rotation runbook"
OPS_RUNBOOK = "look up the ops-database-failover runbook"
SCALE_IN_WINDOW = "scale the catalog-cache deployment to 2 replicas"
SCALE_OUT_OF_WINDOW = "scale the checkout-api deployment to 5 replicas"
ROLLBACK_APPROVED = "roll back the checkout-api deployment for an approving board"
ROLLBACK_DENIED = "roll back the checkout-api deployment for a denying board"
ROLLBACK_SILENT = "roll back the checkout-api deployment for a silent board"

# The offer actions the runtime renders (`appa-runtime/src/engine.rs`,
# `remedy_instruction`). A `{"remedy": ...}` turn names one of them.
ACCEPT = "Accept this change"
APPROVAL = "Request approval"

# The reserved call's answer when a plan releases the call
# (`appa-runtime/src/mcp.rs`, `render`).
AUTHORIZED = "Authorized"

# One offer id as blocking feedback quotes it. A case reads the ids the
# gate opened, so a remedy is checked against the offers of this session.
OFFER_ID = re.compile(r'execute_remedy_plan\(offer_id: "([a-f0-9]+)"\)')

# The ops runbook the annotator confines to the ops readers
# (`demo/demo_tools.py`, `RUNBOOKS`).
OPS_RUNBOOK_TEXT = "Promote the replica"


def test_a_chat_steer_accepts_the_narrowing_and_the_raw_value_reaches_the_agent(stack):
    """The operator steers in a second message. The agent takes the
    narrowing offer instead of the sanitizer, the session narrows to the
    ops readers, and the re-proposed read hands the model the real
    secret. Accepting the narrowing is the remedy that spends the
    session's reach, and the value flows only after it is spent."""
    opening = stack.say(
        STEER_ACCEPT,
        [
            {"tool": "read_secret", "args": {"name": "payments-provider"}},
            {"text": "The read was blocked. Tell me which remedy to take."},
            # The turns below play on the steering message. One script
            # spans both, because the transcript carries the count.
            {"remedy": ACCEPT},
            {"tool": "read_secret", "args": {"name": "payments-provider"}},
            {"text": "I accepted the change and read the secret."},
        ],
    )
    assert opening.state == "completed"
    blocked = opening.responses("read_secret")
    assert blocked, "the read answered the model"
    assert blocked[0].get("appa") == "denied", f"the confidential read is denied first: {blocked[0]}"
    offers = OFFER_ID.findall(str(blocked[0].get("result", "")))
    assert offers, f"the deny quotes runnable offer ids: {blocked[0]}"
    assert not opening.calls("execute_remedy_plan"), "the agent waits for the steer"
    assert SECRET not in opening.everything(), "nothing crosses before the steer"

    steered = stack.agent.say(
        "accept the offered change for this session, then read the secret again",
        opening.context_id,
    )
    assert steered.state == "completed"
    taken = steered.calls("execute_remedy_plan")
    assert taken, "the steered agent called the reserved tool"
    assert taken[0]["args"]["offer_id"] in offers, f"the agent took an offer this session opened: {taken[0]}"
    remedies = steered.responses("execute_remedy_plan")
    assert remedies, "the reserved call answered the model"
    assert AUTHORIZED in json.dumps(remedies), f"the accepted narrowing releases the call: {remedies}"
    reads = steered.responses("read_secret")
    assert reads, "the re-proposed read answered the model"
    assert reads[-1].get("appa") is None, f"the re-proposed read is not gated shut: {reads[-1]}"
    assert SECRET in json.dumps(reads[-1]), f"the narrowed session admits the raw value: {reads[-1]}"
    assert steered.confirmation() is None, "a remedy the agent may take itself asks nobody"


def test_a_chat_steer_declines_every_remedy_and_nothing_crosses(stack):
    """The same block, the opposite steer. The operator tells the agent to
    take nothing, so no plan runs, the offers stay open, and the secret
    never enters the agent."""
    opening = stack.say(
        STEER_DECLINE,
        [
            {"tool": "read_secret", "args": {"name": "payments-provider"}},
            {"text": "The read was blocked and the offers stand."},
            {"text": "I took no remedy, so the offers still stand."},
        ],
    )
    assert opening.state == "completed"
    blocked = opening.responses("read_secret")
    assert blocked, "the read answered the model"
    assert blocked[0].get("appa") == "denied", f"the confidential read is denied: {blocked[0]}"
    offers = OFFER_ID.findall(str(blocked[0].get("result", "")))
    assert offers, f"the deny quotes runnable offer ids: {blocked[0]}"

    steered = stack.agent.say(
        "take no remedy at all and leave the offers standing",
        opening.context_id,
    )
    assert steered.state == "completed"
    assert not steered.calls("execute_remedy_plan"), "the steered agent ran no plan"
    assert not steered.calls("read_secret"), "and proposed no further read"
    assert not any(offer in steered.everything() for offer in offers), "no offer of this session was spent"
    assert SECRET not in opening.everything(), "no secret material crosses the gate"
    assert SECRET not in steered.everything(), "and none crosses after the steer"
    assert steered.confirmation() is None, "a declined remedy reaches no person"


def test_the_human_review_authority_approves_and_the_restart_runs(stack):
    """The effectful restart requires a person's attention. APPA denies
    the call, the agent takes the offered plan, and the plan asks the
    reviewer through kagent's own confirmation. The A2A task suspends,
    the caller rules, and the ruling releases the exact call."""
    task = stack.say(
        RESTART_APPROVED,
        [
            {"tool": "restart_deployment", "args": {"name": "checkout-api"}},
            {"remedy": APPROVAL},
            # The turns below play on the resumed task. The ruling
            # releases the call, and the agent proposes it again.
            {"tool": "restart_deployment", "args": {"name": "checkout-api"}},
            {"text": "The reviewer approved, so the deployment is back up."},
        ],
    )
    assert task.state == "input-required", f"the reviewed remedy suspends the task: {task.state}"
    request = task.confirmation()
    assert request is not None, "the confirmation request is on the wire"
    hint = ((request.get("args") or {}).get("toolConfirmation") or {}).get("hint", "")
    assert "restart_deployment" in hint, f"the person reads the consult artifact, not the model: {hint!r}"
    assert "checkout-api" in hint, f"the artifact names the call under review: {hint!r}"
    blocked = task.responses("restart_deployment")
    assert blocked, "the call answered the model"
    assert blocked[0].get("appa") == "denied", f"the effectful call is denied first: {blocked[0]}"
    assert "requires attention: human-approval" in str(blocked[0].get("result", "")), (
        f"the deny names the missing attention: {blocked[0]}"
    )

    done = stack.agent.decide(task, "approve")
    assert done.state == "completed", f"the resumed task finishes: {done.state}"
    remedies = done.responses("execute_remedy_plan")
    assert remedies, "the resumed reserved call answered the model"
    assert AUTHORIZED in json.dumps(remedies), f"the person's ruling releases the call: {remedies}"
    restarts = done.responses("restart_deployment")
    assert restarts, "the re-proposed restart answered the model"
    assert "restarted" in json.dumps(restarts[-1]), f"the approval runs the restart: {restarts[-1]}"


def test_the_human_review_authority_rejects_and_the_restart_stays_blocked(stack):
    """The same conversation, the opposite ruling. The rejection is the
    authority's answer, so no plan is spent, the re-proposed call is
    denied again, and the deployment is never restarted."""
    task = stack.say(
        RESTART_REJECTED,
        [
            {"tool": "restart_deployment", "args": {"name": "checkout-api"}},
            {"remedy": APPROVAL},
            {"tool": "restart_deployment", "args": {"name": "checkout-api"}},
            {"text": "The reviewer refused, so I made no change."},
        ],
    )
    assert task.state == "input-required", f"the reviewed remedy suspends the task: {task.state}"
    assert task.confirmation() is not None, "the confirmation request is on the wire"

    done = stack.agent.decide(task, "reject")
    assert done.state == "completed", f"the resumed task finishes: {done.state}"
    remedies = done.responses("execute_remedy_plan")
    assert remedies, "the resumed reserved call answered the model"
    assert AUTHORIZED not in json.dumps(remedies), f"a rejection releases nothing: {remedies}"
    restarts = done.responses("restart_deployment")
    assert restarts, "the call answered the model"
    for restart in restarts:
        body = json.dumps(restart)
        assert restart.get("appa") == "denied", f"the restart stays blocked: {body}"
        assert "restarted" not in body, f"the cluster action never ran: {body}"


def test_the_annotator_lets_a_public_runbook_through(stack):
    """The lookup carries no static contract. The registered Annotator
    answers one per call from the runbook id, and a public id gets the
    neutral contract, so the text reaches the model ungated."""
    task = stack.say(
        PUBLIC_RUNBOOK,
        [
            {"tool": "lookup_runbook", "args": {"runbook": "public-oncall-rotation"}},
            {"text": "Page the on-call through the rotation schedule."},
        ],
    )
    assert task.state == "completed"
    responses = task.responses("lookup_runbook")
    assert responses, "the lookup answered the model"
    body = json.dumps(responses[0])
    assert responses[0].get("appa") is None, f"a public runbook is not gated shut: {body}"
    assert "escalate after 15 minutes" in body, f"the real runbook text reaches the model: {body}"
    assert task.confirmation() is None, "nobody is asked about a public runbook"


def test_the_annotator_confines_an_ops_runbook_at_the_read(stack):
    """The same tool, the same session, a different argument. The
    Annotator answers the ops id with a contract that narrows the produced
    value to the ops readers, so admitting it would narrow this public
    session. APPA denies the read and the runbook text never enters."""
    task = stack.say(
        OPS_RUNBOOK,
        [
            {"tool": "lookup_runbook", "args": {"runbook": "ops-database-failover"}},
            {"text": "The lookup was blocked, so I have nothing to report."},
        ],
    )
    assert task.state == "completed"
    responses = task.responses("lookup_runbook")
    assert responses, "the lookup answered the model"
    denied = responses[0]
    assert denied.get("appa") == "denied", f"the per-call contract gates the read: {denied}"
    feedback = str(denied.get("result", ""))
    assert "allowed readers would narrow" in feedback, f"the deny names the narrowing: {feedback}"
    assert "execute_remedy_plan" in feedback, f"the deny carries a runnable offer: {feedback}"
    assert OPS_RUNBOOK_TEXT not in task.everything(), "the ops runbook text never reaches the model"


def test_the_release_window_authority_approves_the_in_window_scale(stack):
    """A human-less authority rules per call. The scale is denied for a
    missing attention mark, the agent takes the offered plan, the runtime
    consults the release-window bot over HTTP, and the approval releases
    the call. No person is ever asked."""
    task = stack.say(
        SCALE_IN_WINDOW,
        [
            {"tool": "scale_deployment", "args": {"name": "catalog-cache", "replicas": 2}},
            {"remedy": APPROVAL},
            {"tool": "scale_deployment", "args": {"name": "catalog-cache", "replicas": 2}},
            {"text": "The release window is open, so the change is live."},
        ],
    )
    assert task.state == "completed"
    assert task.confirmation() is None, "a human-less authority asks no person"
    scales = task.responses("scale_deployment")
    assert len(scales) == 2, f"the call was denied, then proposed again: {scales}"
    assert scales[0].get("appa") == "denied", f"the effectful call is denied first: {scales[0]}"
    assert "requires attention: release-window" in str(scales[0].get("result", "")), (
        f"the deny names the missing attention: {scales[0]}"
    )
    remedies = task.responses("execute_remedy_plan")
    assert remedies, "the reserved call answered the model"
    assert AUTHORIZED in json.dumps(remedies), f"the bot's approval releases the call: {remedies}"
    assert scales[1].get("appa") is None, f"the re-proposed scale is not gated shut: {scales[1]}"
    assert "scaled" in json.dumps(scales[1]), f"the approved scale runs: {scales[1]}"


def test_the_release_window_authority_denies_the_out_of_window_scale(stack):
    """The same tool and the same plan, one deployment outside the window.
    The bot denies, so the plan is spent on nothing, the re-proposed call
    is denied again, and no deployment is scaled."""
    task = stack.say(
        SCALE_OUT_OF_WINDOW,
        [
            {"tool": "scale_deployment", "args": {"name": "checkout-api", "replicas": 5}},
            {"remedy": APPROVAL},
            {"tool": "scale_deployment", "args": {"name": "checkout-api", "replicas": 5}},
            {"text": "The release window is closed, so I made no change."},
        ],
    )
    assert task.state == "completed"
    assert task.confirmation() is None, "a human-less authority asks no person"
    remedies = task.responses("execute_remedy_plan")
    assert remedies, "the reserved call answered the model"
    assert AUTHORIZED not in json.dumps(remedies), f"a denial releases nothing: {remedies}"
    scales = task.responses("scale_deployment")
    assert scales, "the call answered the model"
    for scale in scales:
        body = json.dumps(scale)
        assert scale.get("appa") == "denied", f"the scale stays blocked: {body}"
        assert "scaled" not in body, f"the cluster action never ran: {body}"


def test_the_remote_change_board_approves_and_the_rollback_runs(stack, board):
    """An authority backed by people out of band. The consult parks at the
    change board while the task runs, a member rules on the board's own
    channel, and the ruling releases the exact call. The A2A task never
    suspends, because the person sits on the remote side."""
    member = board.rule_in_background("rollback_deployment", "approve")
    task = stack.say(
        ROLLBACK_APPROVED,
        [
            {"tool": "rollback_deployment", "args": {"name": "checkout-api"}},
            {"remedy": APPROVAL},
            {"tool": "rollback_deployment", "args": {"name": "checkout-api"}},
            {"text": "The board approved, so the previous revision is live."},
        ],
    )
    member.join(5)
    assert task.state == "completed"
    assert task.confirmation() is None, "the board rules out of band, so the caller is never asked"
    rollbacks = task.responses("rollback_deployment")
    assert len(rollbacks) == 2, f"the call was denied, then proposed again: {rollbacks}"
    assert rollbacks[0].get("appa") == "denied", f"the effectful call is denied first: {rollbacks[0]}"
    assert "requires attention: change-approval" in str(rollbacks[0].get("result", "")), (
        f"the deny names the missing attention: {rollbacks[0]}"
    )
    remedies = task.responses("execute_remedy_plan")
    assert remedies, "the reserved call answered the model"
    assert AUTHORIZED in json.dumps(remedies), f"the board's ruling releases the call: {remedies}"
    assert rollbacks[1].get("appa") is None, f"the re-proposed rollback is not gated shut: {rollbacks[1]}"
    assert "rolled_back" in json.dumps(rollbacks[1]), f"the approved rollback runs: {rollbacks[1]}"


def test_the_remote_change_board_denies_and_the_rollback_stays_blocked(stack, board):
    """The same conversation, a denying board. The ruling is the board's,
    so the plan grants nothing and the deployment is never rolled back."""
    member = board.rule_in_background("rollback_deployment", "deny")
    task = stack.say(
        ROLLBACK_DENIED,
        [
            {"tool": "rollback_deployment", "args": {"name": "checkout-api"}},
            {"remedy": APPROVAL},
            {"tool": "rollback_deployment", "args": {"name": "checkout-api"}},
            {"text": "The board refused, so I made no change."},
        ],
    )
    member.join(5)
    assert task.state == "completed"
    assert task.confirmation() is None, "the board rules out of band, so the caller is never asked"
    remedies = task.responses("execute_remedy_plan")
    assert remedies, "the reserved call answered the model"
    assert AUTHORIZED not in json.dumps(remedies), f"a denial releases nothing: {remedies}"
    rollbacks = task.responses("rollback_deployment")
    assert rollbacks, "the call answered the model"
    for rollback in rollbacks:
        body = json.dumps(rollback)
        assert rollback.get("appa") == "denied", f"the rollback stays blocked: {body}"
        assert "rolled_back" not in body, f"the cluster action never ran: {body}"


def test_an_unanswered_change_board_grants_nothing(stack):
    """Nobody rules within the board's approval window, so the consult
    answers nothing. Silence is not an approval. The runtime says the
    offer still stands, and the rollback stays blocked."""
    task = stack.say(
        ROLLBACK_SILENT,
        [
            {"tool": "rollback_deployment", "args": {"name": "checkout-api"}},
            {"remedy": APPROVAL},
            {"tool": "rollback_deployment", "args": {"name": "checkout-api"}},
            {"text": "The board stayed silent, so I made no change."},
        ],
    )
    assert task.state == "completed"
    assert task.confirmation() is None, "an unanswered consult asks the caller nothing"
    remedies = task.responses("execute_remedy_plan")
    assert remedies, "the reserved call answered the model"
    body = json.dumps(remedies)
    assert AUTHORIZED not in body, f"silence releases nothing: {body}"
    assert "gave no answer" in body, f"the runtime names the silence and keeps the offer: {body}"
    rollbacks = task.responses("rollback_deployment")
    assert rollbacks, "the call answered the model"
    for rollback in rollbacks:
        one = json.dumps(rollback)
        assert rollback.get("appa") == "denied", f"the rollback stays blocked: {one}"
        assert "rolled_back" not in one, f"the cluster action never ran: {one}"
