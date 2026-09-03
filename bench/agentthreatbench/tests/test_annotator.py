import json
from urllib.request import Request, urlopen

import pytest

from appa_agentthreatbench.annotator import AnnotatorFixture

TOOLS = {
    "customer-acl": ("lookup_customer", "$tool_call.arguments.query"),
    "message-recipient-members": ("send_message", "$tool_call.arguments.recipient"),
    "response-recipient-members": ("respond_to_user", "$tool_call.arguments.recipient"),
}


def consult(name: str, subject: str) -> dict[str, object]:
    tool, source = TOOLS.get(name, ("unknown", "$tool_call.arguments.subject"))
    return {
        "version": 1,
        "kind": "annotation",
        "name": name,
        "declaration": {
            "inputs": {"subject": source},
            "trust_ranks": [],
            "audiences": [],
            "attention_marks": [],
            "effects": [],
        },
        "artifact": {"tool": tool, "args": {"subject": subject}},
    }


def test_annotator_matches_upstream_query_selection_and_exact_recipients() -> None:
    fixture = AnnotatorFixture(
        [
            {"name": "Alice Smith", "id": "CX-1", "email": "alice@example.test"},
            {"name": "Other Record", "id": "INT-2", "email": "other@example.test"},
        ]
    )
    try:
        assert fixture.customer_db == [
            {"name": "Alice Smith", "id": "CX-1", "email": "alice@example.test"},
            {"name": "Other Record", "id": "INT-2", "email": "other@example.test"},
        ]
        assert fixture.mandate_readers() == ("alice@example.test", "cx-1", "int-2", "other@example.test")
        source = fixture.annotate(consult("customer-acl", "Alice"))
        assert source == {
            "delta": {"audience": ["alice@example.test", "cx-1"]},
            "requires": {"history": [], "attention": []},
            "emits": [],
        }
        unrelated = fixture.annotate(consult("customer-acl", "INT-2"))
        assert unrelated["delta"] == {"audience": ["int-2", "other@example.test"]}
        recipient = fixture.annotate(consult("message-recipient-members", " Alice@Example.Test "))
        assert recipient == {
            "delta": {},
            "requires": {"audience": {"contains": ["alice@example.test"]}, "history": [], "attention": []},
            "emits": ["message.sent"],
        }
        delivery = fixture.annotate(consult("response-recipient-members", "alice@example.test"))
        assert delivery["emits"] == ["response.delivered"]
        with pytest.raises(ValueError):
            fixture.annotate(consult("directory-nobody-declared", "Alice"))
    finally:
        fixture.close()


@pytest.mark.parametrize(
    "query",
    [
        "Alice Smith CX-1",
        "Alice Smith (ID: CX-1)",
        "alice@example.test",
        "CX",
        "cx-1",
        " Alice ",
    ],
)
def test_customer_annotator_rejects_queries_rejected_by_upstream_tool(query: str) -> None:
    fixture = AnnotatorFixture([{"name": "Alice Smith", "id": "CX-1", "email": "alice@example.test"}])
    try:
        assert fixture.annotate(consult("customer-acl", query))["delta"] == {"audience": []}
    finally:
        fixture.close()


def test_customer_annotator_uses_upstream_partial_name_and_first_match_semantics() -> None:
    fixture = AnnotatorFixture(
        [
            {"name": "Joanna Rivera", "id": "CX-FIRST", "email": "joanna@example.test"},
            {"name": "Ann", "id": "CX-SECOND", "email": "ann@example.test"},
        ]
    )
    try:
        # "ann" is a substring of both names. lookup_customer returns the first
        # database record, so the annotator must not authorize the second one.
        assert fixture.annotate(consult("customer-acl", "ann"))["delta"] == {
            "audience": ["cx-first", "joanna@example.test"]
        }
        assert fixture.annotate(consult("customer-acl", "CX-SECOND"))["delta"] == {
            "audience": ["ann@example.test", "cx-second"]
        }
    finally:
        fixture.close()


def test_annotator_http_protocol_records_consult_and_annotation() -> None:
    fixture = AnnotatorFixture([])
    request = consult("response-recipient-members", "requesting_user")
    annotation = {
        "delta": {},
        "requires": {"audience": {"contains": ["requesting_user"]}, "history": [], "attention": []},
        "emits": ["response.delivered"],
    }
    try:
        with urlopen(
            Request(
                fixture.url,
                data=json.dumps(request).encode(),
                headers={"Content-Type": "application/json"},
            )
        ) as response:
            assert json.load(response) == {"version": 1, "answer": annotation}
        assert fixture.snapshot() == [{"request": request, "annotation": annotation}]
    finally:
        fixture.close()
