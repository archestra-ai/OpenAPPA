import asyncio
from types import SimpleNamespace

import pytest
from agent_framework import ChatContext, ChatResponse

from corp_fides.usage import UsageCollector, UsageMiddleware


def test_collector_preserves_provider_token_subtotals_and_cost() -> None:
    collector = UsageCollector()
    collector.record(
        ChatResponse(
            usage_details={
                "input_token_count": 101,
                "output_token_count": 23,
                "total_token_count": 124,
                "cache_read_input_token_count": 40,
                "cache_creation_input_token_count": 7,
                "reasoning_output_token_count": 11,
            },
            raw_representation=SimpleNamespace(
                model_dump=lambda: {"usage": {"cost": 0.0042}}
            ),
        )
    )

    assert collector.summary() == {
        "model_calls": 1,
        "usage_reported_calls": 1,
        "input_tokens": 101,
        "output_tokens": 23,
        "total_tokens": 124,
        "cached_input_tokens": 40,
        "cache_write_input_tokens": 7,
        "reasoning_tokens": 11,
        "cost_usd": pytest.approx(0.0042),
    }


def test_middleware_counts_each_physical_chat_client_call() -> None:
    collector = UsageCollector()
    middleware = UsageMiddleware(collector)
    context = ChatContext(client=SimpleNamespace(), messages=[], options={})

    async def call_next() -> None:
        context.result = ChatResponse(
            usage_details={
                "input_token_count": 9,
                "output_token_count": 4,
                "total_token_count": 13,
            }
        )

    asyncio.run(middleware.process(context, call_next))

    assert collector.summary()["model_calls"] == 1
    assert collector.summary()["total_tokens"] == 13
    assert collector.summary()["cost_usd"] is None
