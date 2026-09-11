"""Provider-reported model usage captured at the chat-client boundary."""

from __future__ import annotations

from dataclasses import dataclass
from numbers import Real
from typing import Any

from agent_framework import ChatContext, ChatMiddleware, ChatResponse, ResponseStream


def _number(value: object) -> int | float | None:
    return value if isinstance(value, Real) and not isinstance(value, bool) else None


def _cost_usd(raw: object) -> float | None:
    values = raw if isinstance(raw, list) else [raw]
    for value in reversed(values):
        if value is None:
            continue
        dumped: Any = value.model_dump() if hasattr(value, "model_dump") else value
        if not isinstance(dumped, dict):
            continue
        usage = dumped.get("usage")
        candidates = [dumped.get("cost"), dumped.get("provider_cost")]
        if isinstance(usage, dict):
            candidates.extend([usage.get("cost"), usage.get("provider_cost")])
        for candidate in candidates:
            number = _number(candidate)
            if number is not None:
                return float(number)
    return None


@dataclass(frozen=True)
class ModelCallUsage:
    input_tokens: int
    output_tokens: int
    total_tokens: int
    cached_input_tokens: int | None
    cache_write_input_tokens: int | None
    reasoning_tokens: int | None
    cost_usd: float | None


class UsageCollector:
    def __init__(self) -> None:
        self.calls: list[ModelCallUsage | None] = []

    def record(self, response: ChatResponse) -> None:
        details = response.usage_details
        required = (
            None
            if details is None
            else (
                _number(details.get("input_token_count")),
                _number(details.get("output_token_count")),
                _number(details.get("total_token_count")),
            )
        )
        if required is None or any(value is None for value in required):
            self.calls.append(None)
            return
        self.calls.append(
            ModelCallUsage(
                input_tokens=int(required[0]),
                output_tokens=int(required[1]),
                total_tokens=int(required[2]),
                cached_input_tokens=self._optional_int(
                    details, "cache_read_input_token_count"
                ),
                cache_write_input_tokens=self._optional_int(
                    details, "cache_creation_input_token_count"
                ),
                reasoning_tokens=self._optional_int(
                    details, "reasoning_output_token_count"
                ),
                cost_usd=_cost_usd(response.raw_representation),
            )
        )

    @staticmethod
    def _optional_int(details: dict[str, Any], key: str) -> int | None:
        value = _number(details.get(key))
        return None if value is None else int(value)

    def summary(self) -> dict[str, int | float | None]:
        reported = [call for call in self.calls if call is not None]

        def optional_sum(field: str) -> int | float | None:
            values = [getattr(call, field) for call in reported]
            return (
                sum(values)
                if len(values) == len(self.calls)
                and all(value is not None for value in values)
                else None
            )

        return {
            "model_calls": len(self.calls),
            "usage_reported_calls": len(reported),
            "input_tokens": sum(call.input_tokens for call in reported),
            "output_tokens": sum(call.output_tokens for call in reported),
            "total_tokens": sum(call.total_tokens for call in reported),
            "cached_input_tokens": optional_sum("cached_input_tokens"),
            "cache_write_input_tokens": optional_sum("cache_write_input_tokens"),
            "reasoning_tokens": optional_sum("reasoning_tokens"),
            "cost_usd": optional_sum("cost_usd"),
        }


class UsageMiddleware(ChatMiddleware):
    def __init__(self, collector: UsageCollector) -> None:
        self.collector = collector

    async def process(self, context: ChatContext, call_next) -> None:
        await call_next()
        if isinstance(context.result, ChatResponse):
            self.collector.record(context.result)
        elif isinstance(context.result, ResponseStream):

            async def capture(response: ChatResponse) -> ChatResponse:
                self.collector.record(response)
                return response

            context.stream_result_hooks.append(capture)
