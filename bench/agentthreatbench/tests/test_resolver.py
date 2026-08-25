import json
from urllib.request import Request, urlopen

import pytest

from appa_agentthreatbench.resolver import ResolverFixture


def customer_request(query: str) -> dict[str, object]:
    return {"version": 1, "resolver": "customer-acl", "args": {"subject": query}}


def test_resolver_matches_upstream_query_selection_and_exact_recipients() -> None:
    fixture = ResolverFixture(
        [
            {"name": "Alice Smith", "id": "CX-1", "email": "alice@example.test"},
            {"name": "Other Record", "id": "INT-2", "email": "other@example.test"},
        ],
        ["alice@example.test", "CX-1", "fraud@example.test"],
    )
    try:
        assert fixture.customer_db == [
            {"name": "Alice Smith", "id": "CX-1", "email": "alice@example.test"},
            {"name": "Other Record", "id": "INT-2", "email": "other@example.test"},
        ]
        source = fixture.resolve(customer_request("Alice"))
        assert source == ["alice@example.test", "cx-1", "fraud@example.test"]
        unrelated = fixture.resolve(customer_request("INT-2"))
        assert unrelated == ["int-2", "other@example.test"]
        recipient = fixture.resolve(
            {
                "version": 1,
                "resolver": "recipient-members",
                "args": {"subject": " Alice@Example.Test "},
            }
        )
        assert recipient == ["alice@example.test"]
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
def test_customer_resolver_rejects_queries_rejected_by_upstream_tool(query: str) -> None:
    fixture = ResolverFixture(
        [{"name": "Alice Smith", "id": "CX-1", "email": "alice@example.test"}],
        ["alice@example.test", "CX-1", "fraud@example.test"],
    )
    try:
        assert fixture.resolve(customer_request(query)) == []
    finally:
        fixture.close()


def test_customer_resolver_uses_upstream_partial_name_and_first_match_semantics() -> None:
    fixture = ResolverFixture(
        [
            {"name": "Joanna Rivera", "id": "CX-FIRST", "email": "joanna@example.test"},
            {"name": "Ann", "id": "CX-SECOND", "email": "ann@example.test"},
        ],
        ["CX-SECOND", "ann@example.test", "fraud@example.test"],
    )
    try:
        # "ann" is a substring of both names. lookup_customer returns the first
        # database record, so the resolver must not authorize the second one.
        assert fixture.resolve(customer_request("ann")) == ["cx-first", "joanna@example.test"]
        assert fixture.resolve(customer_request("CX-SECOND")) == [
            "ann@example.test",
            "cx-second",
            "fraud@example.test",
        ]
    finally:
        fixture.close()


def test_resolver_http_protocol_records_request_and_answer() -> None:
    fixture = ResolverFixture([], [])
    request = {
        "version": 1,
        "resolver": "recipient-members",
        "args": {"subject": "requesting_user"},
    }
    try:
        with urlopen(
            Request(
                fixture.url,
                data=json.dumps(request).encode(),
                headers={"Content-Type": "application/json"},
            )
        ) as response:
            assert json.load(response) == {
                "version": 1,
                "result": {"requires.audience": {"contains": ["requesting_user"]}},
            }
        assert fixture.snapshot() == [{"request": request, "readers": ["requesting_user"]}]
    finally:
        fixture.close()
