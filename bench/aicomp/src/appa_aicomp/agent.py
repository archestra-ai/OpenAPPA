"""OpenRouter chat-completions agent for the aicomp_sdk sandbox."""

import json
import logging
import os
import random
import time
import uuid
from collections.abc import Sequence
from typing import Any

import openai
from aicomp_sdk.agents.tool_specs import build_openai_tool_name_maps
from aicomp_sdk.agents.types import (
    AGENT_STATE_VERSION,
    AgentDecision,
    AgentStateSnapshot,
    AgentToolSpec,
    AssistantMessageEvent,
    FinalResponseDecision,
    InvalidModelOutputError,
    ToolCall,
    ToolCallDecision,
    ToolRequestEvent,
    ToolResultEvent,
    UserMessageEvent,
)
from aicomp_sdk.core.runtime_history import RuntimeHistory

logger = logging.getLogger(__name__)

# A hung request is retried by `_complete`, not waited out.
REQUEST_TIMEOUT_S = 120
OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1"


class OpenRouterAgent:
    def __init__(self, model: str, client: openai.OpenAI | None = None, max_retries: int = 4) -> None:
        self._model = model
        self._client = client or openai.OpenAI(
            base_url=OPENROUTER_BASE_URL, api_key=os.environ["OPENROUTER_API_KEY"], timeout=REQUEST_TIMEOUT_S, max_retries=0
        )
        self._max_retries = max_retries
        self.calls = 0
        self.invalid_outputs = 0

    def next_action(self, *, history: RuntimeHistory, tools: Sequence[AgentToolSpec]) -> AgentDecision:
        to_alias, from_alias = build_openai_tool_name_maps(tools)
        request = {
            "model": self._model,
            "messages": self._messages(history, to_alias),
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": to_alias[spec.name],
                        "description": spec.description,
                        "parameters": spec.parameters_json_schema,
                    },
                }
                for spec in tools
            ],
            "parallel_tool_calls": False,
            "temperature": 0,
        }
        message = self._complete(request)
        try:
            return self._decide(message, from_alias)
        except InvalidModelOutputError:
            self.invalid_outputs += 1
            raise

    @staticmethod
    def _decide(message: Any, from_alias: dict[str, str]) -> AgentDecision:
        tool_calls = message.tool_calls or []
        if not tool_calls:
            return FinalResponseDecision(text=message.content or "")
        call = tool_calls[0]
        name = from_alias.get(call.function.name)
        if name is None:
            raise InvalidModelOutputError(f"unknown tool alias {call.function.name!r}")
        try:
            arguments = json.loads(call.function.arguments or "{}")
        except json.JSONDecodeError as error:
            raise InvalidModelOutputError(f"tool arguments are not JSON: {error}") from error
        if not isinstance(arguments, dict):
            raise InvalidModelOutputError("tool arguments are not an object")
        return ToolCallDecision(
            call=ToolCall(call_id=call.id or f"call_{uuid.uuid4().hex[:12]}", tool_name=name, arguments=arguments),
            assistant_message=message.content or None,
        )

    def _complete(self, request: dict[str, Any]) -> Any:
        for attempt in range(self._max_retries + 1):
            try:
                self.calls += 1
                response = self._client.chat.completions.create(**request)
                if not response.choices:
                    raise InvalidModelOutputError("empty choices")
                return response.choices[0].message
            except (openai.APIConnectionError, openai.RateLimitError, openai.InternalServerError, InvalidModelOutputError):
                if attempt == self._max_retries:
                    raise
                logger.exception("OpenRouter call failed, retrying")
                time.sleep(2**attempt * random.uniform(0.5, 1.5))
        raise AssertionError("unreachable")

    @staticmethod
    def _messages(history: RuntimeHistory, to_alias: dict[str, str]) -> list[dict[str, Any]]:
        instructions = "\n\n".join(event.text for event in history.instructions if event.text.strip())
        messages: list[dict[str, Any]] = [{"role": "system", "content": instructions}] if instructions else []
        for event in history.events:
            match event:
                case UserMessageEvent(text=text):
                    messages.append({"role": "user", "content": text})
                case AssistantMessageEvent(text=text):
                    messages.append({"role": "assistant", "content": text})
                case ToolRequestEvent(call=call):
                    messages.append(
                        {
                            "role": "assistant",
                            "content": None,
                            "tool_calls": [
                                {
                                    "id": call.call_id,
                                    "type": "function",
                                    "function": {
                                        "name": to_alias.get(call.tool_name, call.tool_name),
                                        "arguments": json.dumps(call.arguments, sort_keys=True),
                                    },
                                }
                            ],
                        }
                    )
                case ToolResultEvent(result=result):
                    messages.append({"role": "tool", "tool_call_id": result.call_id, "content": result.output_text})
                case _:
                    raise InvalidModelOutputError(f"unsupported runtime event: {event!r}")
        return messages

    def reset_state(self) -> None:
        pass

    def snapshot_state(self) -> AgentStateSnapshot:
        return {"version": AGENT_STATE_VERSION, "backend": "openrouter_chat", "data": {}}

    def restore_state(self, snapshot: AgentStateSnapshot) -> None:
        pass
