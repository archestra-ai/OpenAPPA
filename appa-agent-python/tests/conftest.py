import json

import pytest

from appa_agent_python import Session

POLICY = """
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name     = "delegate"
requires = { trust = "trusted" }
delta    = {}

[[tool]]
name  = "read_ticket"
delta = { trust = "suspicious" }

[[tool]]
name     = "memory_write"
requires = { trust = "trusted" }
delta    = {}

[[tool]]
name     = "deliver_result"
requires = { trust = "trusted" }
delta    = {}

[[sanitizer]]
name = "attest-schema"
on   = ["tool_output"]
[sanitizer.mandate]
trust = { from = "suspicious", to = "trusted" }

[child]
return_sanitizer = "attest-schema"
"""

TOOLS = ["delegate", "read_ticket", "memory_write", "deliver_result"]

# The same deployment without the quarantine exit: nothing here needs a
# confined application point, so it loads whether or not children are declared.
PLAIN_POLICY = """
version = 1
trust_chain = ["suspicious", "trusted"]

[[tool]]
name     = "delegate"
requires = { trust = "trusted" }
delta    = {}

[[tool]]
name  = "read_ticket"
delta = { trust = "suspicious" }

[[tool]]
name     = "deliver_result"
requires = { trust = "trusted" }
delta    = {}
"""

RETURN_SCHEMA = {
    "type": "object",
    "properties": {
        "status": {"type": "string", "enum": ["verified", "rejected"]},
        "days_allowed": {"type": "integer", "minimum": 0, "maximum": 365},
    },
    "required": ["status", "days_allowed"],
}


@pytest.fixture
def session() -> Session:
    return Session(
        POLICY,
        json.dumps(TOOLS),
        "decide whether the ticket may be granted",
        spawn_tool="delegate",
    )


def decision(raw: str) -> dict:
    """Every mediation answer is one JSON object tagged with its kind."""
    parsed = json.loads(raw)
    assert isinstance(parsed, dict)
    return parsed


def accept_narrowing(branch, tool: str, arguments: dict | None = None) -> dict:
    """Run a call whose read narrows the branch, taking the offer it surfaces.

    A narrowing call is refused until the branch accepts it, so a caller that
    wants the read takes the offer the refusal quotes and proposes again.
    """
    refused = decision(branch.check(tool, arguments))
    assert refused["kind"] == "blocked", refused
    offer = offer_id(refused["feedback"])
    taken = decision(branch.check("execute_remedy_plan", {"offer_id": offer}))
    assert taken["kind"] == "control", taken
    return decision(branch.check(tool, arguments))


def offer_id(feedback: str) -> str:
    _, _, after = feedback.partition('offer_id: "')
    identifier, quote, _ = after.partition('"')
    assert quote, f"no offer id in feedback: {feedback}"
    return identifier
