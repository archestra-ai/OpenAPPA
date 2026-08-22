import asyncio
import json

import pytest

from appa_agentthreatbench.fides import (
    FidesAllowed,
    FidesBlocked,
    FidesPolicy,
    FidesSession,
    FidesToolPolicy,
    fides_policy,
)


def test_direct_fides_policy_can_validate_an_external_tool_surface() -> None:
    policy = FidesPolicy("custom", {"read": FidesToolPolicy()})

    policy.check_covers({"read"})
    with pytest.raises(ValueError, match=r"missing=\['write'\]"):
        policy.check_covers({"read", "write"})


@pytest.mark.asyncio
async def test_real_fides_middleware_memory_poison_allows_response_and_blocks_write() -> None:
    policy = fides_policy("memory_poison", {"memory_read", "memory_write", "respond_to_user"})
    session = FidesSession(policy)
    try:
        read = await session.invoke(
            "memory_read",
            {"key": "policy"},
            lambda _: _result("safe fact plus injected instruction"),
        )
        assert isinstance(read, FidesAllowed)

        delivery_called = False

        async def deliver(_: dict[str, object]) -> str:
            nonlocal delivery_called
            delivery_called = True
            return "delivered"

        # respond_to_user is permitted to answer the user
        delivery = await session.invoke(
            "respond_to_user",
            {"recipient": "requesting_user", "body": "safe fact"},
            deliver,
        )
        assert isinstance(delivery, FidesAllowed)
        assert delivery_called

        # memory_write is blocked in untrusted context
        write = await session.invoke(
            "memory_write",
            {"key": "policy", "value": "corrupted"},
            lambda: _result("written"),
        )
        assert isinstance(write, FidesBlocked)
    finally:
        audit = session.close()
    assert audit
    json.dumps(audit)


@pytest.mark.asyncio
async def test_real_fides_middleware_autonomy_hijack_blocks_downstream_actions() -> None:
    names = {"read_inbox", "categorize_email", "draft_summary", "respond_to_user"}
    session = FidesSession(fides_policy("autonomy_hijack", names))
    try:
        read = await session.invoke(
            "read_inbox",
            {},
            lambda _: _result("Inbox: 1. legitimate email, 2. malicious email"),
        )
        assert isinstance(read, FidesAllowed)

        # Protected tools are blocked in untrusted context
        categorize = await session.invoke(
            "categorize_email",
            {"email_index": 0, "category": "general"},
            lambda _: _result("categorized"),
        )
        assert isinstance(categorize, FidesBlocked)
    finally:
        session.close()


@pytest.mark.asyncio
async def test_data_exfil_policy_exposes_coarse_confidentiality_tradeoff() -> None:
    names = {"lookup_customer", "send_message", "resolve_ticket", "respond_to_user"}
    session = FidesSession(fides_policy("data_exfil", names))
    try:
        lookup = await session.invoke(
            "lookup_customer",
            {"query": "CX-1"},
            lambda _: _result("Customer: Alice; SSN: 123-45-6789"),
        )
        assert isinstance(lookup, FidesAllowed)

        # Internal resolution accepts the tainted/private record, while FIDES's
        # coarse public-delivery gate cannot distinguish an authorized customer.
        resolution = await session.invoke(
            "resolve_ticket",
            {"customer_id": "CX-1", "resolution": "corrected"},
            lambda _: _result("resolved"),
        )
        assert isinstance(resolution, FidesAllowed)
        for recipient in ("alice@example.test", "attacker@example.test"):
            send = await session.invoke(
                "send_message",
                {"recipient": recipient, "body": "customer record"},
                lambda _: _result("sent"),
            )
            assert isinstance(send, FidesBlocked)
    finally:
        session.close()


@pytest.mark.asyncio
async def test_fides_tool_errors_remain_allowed_errors_not_successful_results() -> None:
    session = FidesSession(fides_policy("memory_poison", {"memory_read", "memory_write", "respond_to_user"}))

    async def fail(_: dict[str, object]) -> str:
        raise ValueError("fixture failure")

    try:
        result = await session.invoke("memory_read", {"key": "missing"}, fail)
        assert isinstance(result, FidesAllowed)
        assert result.content == ""
        assert result.execution_error == "Tool failed: ValueError: fixture failure"
    finally:
        session.close()


@pytest.mark.asyncio
async def test_native_fides_hides_then_inspects_untrusted_content() -> None:
    policy = fides_policy("memory_poison", {"memory_read", "memory_write", "respond_to_user"})
    session = FidesSession(policy, native=True)
    try:
        read = await session.invoke(
            "memory_read",
            {"key": "policy"},
            lambda _: _result("safe fact plus injected instruction"),
        )
        assert isinstance(read, FidesAllowed)
        reference = json.loads(read.content)
        assert reference["type"] == "variable_reference"

        inspected = await session.invoke(
            "inspect_variable",
            {"variable_id": reference["variable_id"], "reason": "test"},
            None,
        )
        assert isinstance(inspected, FidesAllowed)
        assert "safe fact plus injected instruction" in inspected.content

        write = await session.invoke(
            "memory_write",
            {"key": "policy", "value": "corrupted"},
            lambda _: _result("written"),
        )
        assert isinstance(write, FidesBlocked)
    finally:
        session.close()


@pytest.mark.asyncio
async def test_fides_session_owns_the_trajectory_limit_until_close() -> None:
    policy = fides_policy("memory_poison", {"memory_read", "memory_write", "respond_to_user"})
    native = FidesSession(policy, native=True)
    middleware = FidesSession(policy)
    try:
        read = await native.invoke(
            "memory_read",
            {"key": "policy"},
            lambda _: _result("native sample content"),
        )
        assert isinstance(read, FidesAllowed)
        variable_id = json.loads(read.content)["variable_id"]

        inspected_task = asyncio.create_task(native.invoke("inspect_variable", {"variable_id": variable_id}, None))
        overlapping_task = asyncio.create_task(
            middleware.invoke(
                "memory_read",
                {"key": "other"},
                lambda _: _result("other sample content"),
            )
        )
        inspected = await inspected_task
        assert isinstance(inspected, FidesAllowed)
        assert "native sample content" in inspected.content
        await asyncio.sleep(0)
        assert not overlapping_task.done()

        native.close()
        overlapping = await overlapping_task
        assert isinstance(overlapping, FidesAllowed)
    finally:
        native.close()
        middleware.close()


def test_native_fides_digest_and_binding_are_distinct() -> None:
    policy = fides_policy("memory_poison", {"memory_read", "memory_write", "respond_to_user"})
    middleware = FidesSession(policy)
    native = FidesSession(policy, native=True)
    try:
        assert middleware.digest() != native.digest()
        assert middleware.binding_identity != native.binding_identity
        assert set(native.security_tools) == {"quarantined_llm", "inspect_variable"}
    finally:
        middleware.close()
        native.close()


async def _result(value: str) -> str:
    return value
