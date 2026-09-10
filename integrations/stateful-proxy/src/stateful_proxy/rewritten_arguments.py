"""Protocol-specific spawn carrier rewrites for provider tool arguments."""

from __future__ import annotations

import copy
import json
import re
from collections.abc import Callable
from typing import Any


class SpawnArgumentRewriteError(ValueError):
    """A provider exposed a spawn call in a shape that cannot be safely rewritten."""


_SPAWN_FIELDS = {
    "Agent": "prompt",
    "Task": "prompt",
    "spawn_agent": "message",
    "task": "prompt",
}
_SSE_BLOCKS = re.compile(br"(.*?)(\r?\n\r?\n|\Z)", re.DOTALL)


def canonical_spawn_name(name: Any) -> str | None:
    if not isinstance(name, str):
        return None
    for namespace in ("agents:", "multi_agent_v1:"):
        if name.startswith(namespace):
            return name.removeprefix(namespace)
    return name


def is_spawn_tool(name: Any) -> bool:
    return canonical_spawn_name(name) in _SPAWN_FIELDS


def correlation_marker(call_id: str, marker: str) -> str:
    return f'<appa-correlation parent_call="{call_id}" spawn_marker="{marker}"/>'


def rewrite_spawn_args(call_id: str, name: str, args: Any, marker: str) -> dict[str, Any]:
    """Return the sole client-executed spawn argument form for a known call."""
    field = _SPAWN_FIELDS.get(canonical_spawn_name(name))
    if field is None:
        if not isinstance(args, dict):
            raise SpawnArgumentRewriteError("provider spawn arguments must be an object")
        return copy.deepcopy(args)
    if not isinstance(call_id, str) or not call_id or not isinstance(marker, str) or not marker:
        raise SpawnArgumentRewriteError("provider spawn call has no stable carrier identity")
    if not isinstance(args, dict) or not isinstance(args.get(field), str):
        raise SpawnArgumentRewriteError("provider spawn arguments have an unsupported documented shape")
    result = copy.deepcopy(args)
    carrier = correlation_marker(call_id, marker)
    if not result[field].startswith(carrier):
        result[field] = carrier + result[field]
    return result


def rewrite_spawn_arguments_json(call_id: str, name: str, arguments: Any, marker: str) -> str:
    if not isinstance(arguments, str):
        raise SpawnArgumentRewriteError("provider function-call arguments are not a JSON string")
    try:
        value = json.loads(arguments)
    except json.JSONDecodeError as error:
        raise SpawnArgumentRewriteError("provider function-call arguments are incomplete or invalid") from error
    return json.dumps(rewrite_spawn_args(call_id, name, value, marker), separators=(",", ":"), ensure_ascii=True)


def _responses_call(item: Any) -> tuple[str, str, str] | None:
    if not isinstance(item, dict) or item.get("type") != "function_call":
        return None
    call_id, name, arguments = item.get("call_id"), item.get("name"), item.get("arguments")
    if not isinstance(call_id, str) or not isinstance(name, str):
        raise SpawnArgumentRewriteError("Responses function call has no stable call ID and name")
    if not is_spawn_tool(name):
        return None
    if not isinstance(arguments, str):
        raise SpawnArgumentRewriteError("Responses spawn call has no JSON arguments")
    return call_id, name, arguments


def _chat_call(tool_call: Any) -> tuple[str, str, str] | None:
    if not isinstance(tool_call, dict):
        raise SpawnArgumentRewriteError("Chat tool call has an unsupported shape")
    function = tool_call.get("function")
    call_id = tool_call.get("id")
    if not isinstance(function, dict) or not isinstance(call_id, str):
        raise SpawnArgumentRewriteError("Chat tool call has no stable function identity")
    name, arguments = function.get("name"), function.get("arguments")
    if not is_spawn_tool(name):
        return None
    if not isinstance(arguments, str):
        raise SpawnArgumentRewriteError("Chat spawn call has no JSON arguments")
    return call_id, name, arguments


def rewrite_response_arguments(payload: Any, provider: str, marker_for: Callable[[str], str | None]) -> Any:
    """Rewrite complete documented response forms without touching arbitrary data."""
    if provider == "anthropic":
        return copy.deepcopy(payload)
    result = copy.deepcopy(payload)

    def rewrite_item(item: Any) -> None:
        call = _responses_call(item)
        if call is None:
            return
        call_id, name, arguments = call
        marker = marker_for(call_id)
        if marker is None:
            raise SpawnArgumentRewriteError("admitted spawn has no eligible carrier")
        item["arguments"] = rewrite_spawn_arguments_json(call_id, name, arguments, marker)

    if not isinstance(result, dict):
        return result
    for item in result.get("output", []) if isinstance(result.get("output"), list) else []:
        rewrite_item(item)
    response = result.get("response")
    if isinstance(response, dict):
        for item in response.get("output", []) if isinstance(response.get("output"), list) else []:
            rewrite_item(item)
    for choice in result.get("choices", []) if isinstance(result.get("choices"), list) else []:
        if not isinstance(choice, dict):
            continue
        message = choice.get("message")
        for tool_call in message.get("tool_calls", []) if isinstance(message, dict) and isinstance(message.get("tool_calls"), list) else []:
            call = _chat_call(tool_call)
            if call is None:
                continue
            call_id, name, arguments = call
            marker = marker_for(call_id)
            if marker is None:
                raise SpawnArgumentRewriteError("admitted spawn has no eligible carrier")
            tool_call["function"]["arguments"] = rewrite_spawn_arguments_json(call_id, name, arguments, marker)
    return result


def rewrite_sse_arguments(raw: bytes, provider: str, marker_for: Callable[[str], str | None]) -> bytes:
    """Rewrite Responses/Chat spawn deltas after complete arguments have been admitted.

    The first delta for a call carries the complete transformed JSON. Remaining
    deltas are empty, while every documented final representation is updated.
    """
    if provider == "anthropic":
        return raw
    events: list[tuple[bytes, bytes, dict[str, Any] | None]] = []
    response_calls: dict[str, dict[str, Any]] = {}
    chat_calls: dict[int, dict[str, Any]] = {}

    for match in _SSE_BLOCKS.finditer(raw):
        block, ending = match.groups()
        if not block and not ending:
            continue
        data_match = re.search(br"(?m)^data:\s*(.+)$", block)
        if not data_match or data_match.group(1) == b"[DONE]":
            events.append((block, ending, None))
            continue
        try:
            value = json.loads(data_match.group(1))
        except (UnicodeDecodeError, json.JSONDecodeError):
            events.append((block, ending, None))
            continue
        if not isinstance(value, dict):
            events.append((block, ending, None))
            continue
        events.append((block, ending, value))
        item = value.get("item")
        for candidate in (item, *(value.get("output", []) if isinstance(value.get("output"), list) else [])):
            if not isinstance(candidate, dict) or candidate.get("type") != "function_call":
                continue
            call_id, name = candidate.get("call_id"), candidate.get("name")
            if isinstance(call_id, str) and isinstance(name, str) and is_spawn_tool(name):
                state = response_calls.setdefault(call_id, {"name": name, "parts": [], "arguments": None})
                state["name"] = name
                if isinstance(candidate.get("arguments"), str) and candidate["arguments"]:
                    state["arguments"] = candidate["arguments"]
        response = value.get("response")
        for candidate in response.get("output", []) if isinstance(response, dict) and isinstance(response.get("output"), list) else []:
            if isinstance(candidate, dict) and candidate.get("type") == "function_call" and is_spawn_tool(candidate.get("name")):
                call_id = candidate.get("call_id")
                if not isinstance(call_id, str):
                    raise SpawnArgumentRewriteError("Responses completed spawn has no call ID")
                state = response_calls.setdefault(call_id, {"name": candidate["name"], "parts": [], "arguments": None})
                if isinstance(candidate.get("arguments"), str):
                    state["arguments"] = candidate["arguments"]
        if value.get("type") in {"response.function_call_arguments.delta", "response.function_call_arguments.done"}:
            call_id = value.get("call_id")
            if isinstance(call_id, str) and call_id in response_calls:
                key = "delta" if value.get("type").endswith(".delta") else "arguments"
                if not isinstance(value.get(key), str):
                    raise SpawnArgumentRewriteError("Responses spawn argument event has no JSON text")
                if key == "delta":
                    response_calls[call_id]["parts"].append(value[key])
                else:
                    response_calls[call_id]["arguments"] = value[key]
        for choice in value.get("choices", []) if isinstance(value.get("choices"), list) else []:
            delta = choice.get("delta") if isinstance(choice, dict) else None
            for tool_call in delta.get("tool_calls", []) if isinstance(delta, dict) and isinstance(delta.get("tool_calls"), list) else []:
                if not isinstance(tool_call, dict) or not isinstance(tool_call.get("index"), int):
                    raise SpawnArgumentRewriteError("Chat tool call stream has no index")
                index = tool_call["index"]
                state = chat_calls.setdefault(index, {"id": None, "name": None, "parts": []})
                if isinstance(tool_call.get("id"), str):
                    state["id"] = tool_call["id"]
                function = tool_call.get("function")
                if isinstance(function, dict):
                    if isinstance(function.get("name"), str):
                        state["name"] = function["name"]
                    if is_spawn_tool(state["name"]):
                        if not isinstance(function.get("arguments"), str):
                            raise SpawnArgumentRewriteError("Chat spawn delta has no JSON text")
                        state["parts"].append(function["arguments"])

    rewritten: dict[str, str] = {}
    for call_id, state in response_calls.items():
        arguments = state["arguments"] if isinstance(state["arguments"], str) else "".join(state["parts"])
        if not arguments:
            raise SpawnArgumentRewriteError("Responses spawn stream ended without complete arguments")
        marker = marker_for(call_id)
        if marker is None:
            raise SpawnArgumentRewriteError("admitted spawn has no eligible carrier")
        rewritten[call_id] = rewrite_spawn_arguments_json(call_id, state["name"], arguments, marker)
    for state in chat_calls.values():
        if not is_spawn_tool(state["name"]):
            continue
        if not isinstance(state["id"], str) or not state["parts"]:
            raise SpawnArgumentRewriteError("Chat spawn stream ended without complete arguments")
        marker = marker_for(state["id"])
        if marker is None:
            raise SpawnArgumentRewriteError("admitted spawn has no eligible carrier")
        rewritten[state["id"]] = rewrite_spawn_arguments_json(state["id"], state["name"], "".join(state["parts"]), marker)

    emitted_response: set[str] = set()
    emitted_chat: set[str] = set()
    rendered: list[bytes] = []
    for block, ending, value in events:
        if value is None:
            rendered.append(block + ending)
            continue

        def replace_item(item: Any) -> None:
            if isinstance(item, dict) and item.get("type") == "function_call" and isinstance(item.get("call_id"), str) and item["call_id"] in rewritten:
                item["arguments"] = rewritten[item["call_id"]]

        replace_item(value.get("item"))
        for item in value.get("output", []) if isinstance(value.get("output"), list) else []:
            replace_item(item)
        response = value.get("response")
        for item in response.get("output", []) if isinstance(response, dict) and isinstance(response.get("output"), list) else []:
            replace_item(item)
        if value.get("type") in {"response.function_call_arguments.delta", "response.function_call_arguments.done"} and isinstance(value.get("call_id"), str) and value["call_id"] in rewritten:
            call_id = value["call_id"]
            if value["type"].endswith(".delta"):
                value["delta"] = rewritten[call_id] if call_id not in emitted_response else ""
                emitted_response.add(call_id)
            else:
                value["arguments"] = rewritten[call_id]
        for choice in value.get("choices", []) if isinstance(value.get("choices"), list) else []:
            delta = choice.get("delta") if isinstance(choice, dict) else None
            for tool_call in delta.get("tool_calls", []) if isinstance(delta, dict) and isinstance(delta.get("tool_calls"), list) else []:
                if not isinstance(tool_call, dict):
                    continue
                call_id = tool_call.get("id")
                if not isinstance(call_id, str):
                    index = tool_call.get("index")
                    state = chat_calls.get(index) if isinstance(index, int) else None
                    call_id = state["id"] if state else None
                if isinstance(call_id, str) and call_id in rewritten and isinstance(tool_call.get("function"), dict):
                    tool_call["function"]["arguments"] = rewritten[call_id] if call_id not in emitted_chat else ""
                    emitted_chat.add(call_id)
        encoded = json.dumps(value, separators=(",", ":"), ensure_ascii=True).encode("utf-8")
        # A callable replacement preserves JSON backslashes inside argument text.
        rendered.append(re.sub(br"(?m)^data:\s*.+$", lambda _match: b"data: " + encoded, block) + ending)
    return b"".join(rendered)
