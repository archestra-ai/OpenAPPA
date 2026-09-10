"""Extract untrusted client correlation hints from captured HTTP messages.

This module deliberately reports client assertions, not authenticated identity or
authorization facts. It is safe for offline capture-log analysis only.
"""

from __future__ import annotations

import json
import uuid


IDENTITY_FIELDS = (
    "session_id",
    "thread_id",
    "agent_id",
    "parent_session_id",
    "parent_thread_id",
    "parent_turn_id",
    "root_turn_id",
    "turn_id",
    "window_id",
    "window_number",
    "context_window_id",
    "request_kind",
    "request_id",
    "forked_from_thread_id",
    "forked_from_ordinal_exclusive",
    "compaction",
)


def normalize_identity(headers, body):
    """Return client-asserted identity metadata and any contradictory values.

    ``headers`` is a mapping and ``body`` is normally a decoded JSON object. A
    JSON string is accepted for convenience. Missing, malformed, and unknown
    shapes produce an explicit empty, untrusted result instead of a guess.
    """
    headers = _headers(headers)
    body = _object(body)
    client = _client_kind(headers, body)
    result = {
        "client": client,
        "trust": "client_asserted_not_authenticated_authority",
        "identity": {field: None for field in IDENTITY_FIELDS},
        "provenance": {field: None for field in IDENTITY_FIELDS},
        "observed": {field: [] for field in IDENTITY_FIELDS},
        "conflicts": [],
    }

    if client == "codex":
        _codex_identity(result, headers, body)
    elif client == "opencode":
        _opencode_identity(result, headers)
    elif client == "claude":
        _claude_identity(result, headers, body)

    _finalize(result)
    return result


def extract_tool_edges(body):
    """Extract protocol-level tool call/result joins without recursive scanning.

    Only documented Anthropic content blocks, Responses items, and Chat
    Completions tool fields are considered. IDs inside arbitrary tool arguments,
    result text, or application metadata are intentionally ignored.
    """
    body = _object(body)
    calls = []
    results = []

    for node in _anthropic_nodes(body):
        if node.get("type") == "tool_use" and _text(node.get("id")):
            calls.append(_tool_record(node["id"], "anthropic.tool_use"))
        elif node.get("type") == "tool_result" and _text(node.get("tool_use_id")):
            results.append(_tool_record(node["tool_use_id"], "anthropic.tool_result"))

    for node in _responses_nodes(body):
        kind = node.get("type")
        if kind in ("function_call", "custom_tool_call") and _text(node.get("call_id")):
            calls.append(_tool_record(node["call_id"], "responses." + kind))
        elif kind in ("function_call_output", "custom_tool_call_output") and _text(node.get("call_id")):
            results.append(_tool_record(node["call_id"], "responses." + kind))

    for message in _chat_messages(body):
        tool_calls = message.get("tool_calls")
        if isinstance(tool_calls, list):
            for tool_call in tool_calls:
                if isinstance(tool_call, dict) and _text(tool_call.get("id")):
                    calls.append(_tool_record(tool_call["id"], "chat_completions.tool_call"))
        if message.get("role") == "tool" and _text(message.get("tool_call_id")):
            results.append(_tool_record(message["tool_call_id"], "chat_completions.tool_result"))

    edges = []
    orphan_results = []
    call_indexes = {}
    for index, call in enumerate(calls):
        call_indexes.setdefault(call["call_id"], []).append(index)
    for result_index, result in enumerate(results):
        matches = call_indexes.get(result["call_id"], [])
        if not matches:
            orphan_results.append({"result_index": result_index, **result})
            continue
        for call_index in matches:
            edges.append(
                {
                    "call_id": result["call_id"],
                    "call_index": call_index,
                    "result_index": result_index,
                }
            )
    return {"calls": calls, "results": results, "edges": edges, "orphan_results": orphan_results}


def _codex_identity(result, headers, body):
    metadata = _object(body.get("client_metadata"))
    body_canonical = _object(metadata.get("x-codex-turn-metadata"))
    header_canonical = _object(headers.get("x-codex-turn-metadata"))
    fields = {
        "session_id": "session_id",
        "thread_id": "thread_id",
        "parent_thread_id": "parent_thread_id",
        "parent_turn_id": "parent_turn_id",
        "root_turn_id": "root_turn_id",
        "turn_id": "turn_id",
        "window_id": "window_id",
        "window_number": "window_number",
        "context_window_id": "context_window_id",
        "request_kind": "request_kind",
        "forked_from_thread_id": "forked_from_thread_id",
        "forked_from_ordinal_exclusive": "forked_from_ordinal_exclusive",
        "compaction": "compaction",
    }
    for field, key in fields.items():
        _observe(result, field, body_canonical.get(key), "codex.canonical_body")
        _observe(result, field, header_canonical.get(key), "codex.canonical_header")
        _observe(result, field, metadata.get(key), "codex.client_metadata")
        _observe(result, field, body.get(key), "codex.body_flat")
    _observe(result, "session_id", headers.get("session-id"), "codex.header")
    _observe(result, "parent_thread_id", metadata.get("x-codex-parent-thread-id"), "codex.client_metadata")
    _observe(result, "parent_thread_id", headers.get("x-codex-parent-thread-id"), "codex.header")
    _observe(result, "window_id", headers.get("x-codex-window-id"), "codex.header")


def _opencode_identity(result, headers):
    _observe(result, "session_id", headers.get("x-opencode-session"), "opencode.hosted_header")
    _observe(result, "session_id", headers.get("x-session-id"), "opencode.normal_header")
    _observe(result, "session_id", headers.get("x-session-affinity"), "opencode.affinity_header")
    _observe(result, "parent_session_id", headers.get("x-parent-session-id"), "opencode.parent_header")
    _observe(result, "request_id", headers.get("x-opencode-request"), "opencode.hosted_header")


def _claude_identity(result, headers, body):
    metadata = _object(body.get("metadata"))
    _observe(result, "session_id", headers.get("x-claude-code-session-id"), "claude.header.session_id")
    _observe(result, "agent_id", headers.get("x-claude-code-agent-id"), "claude.header.agent_id")
    _observe(result, "session_id", _claude_session_id(metadata.get("user_id")), "claude.metadata.user_id")
    _observe(result, "session_id", _claude_session_id(body.get("user_id")), "claude.legacy_user_id")
    # An agent identity must be explicitly supplied; a session ID never implies it.
    _observe(result, "agent_id", metadata.get("agent_id"), "claude.metadata.agent_id")
    _observe(result, "agent_id", body.get("agent_id"), "claude.explicit_agent_id")


def _observe(result, field, value, source):
    if value is None or value == "":
        return
    result["observed"][field].append({"source": source, "value": value})


def _finalize(result):
    for field, observations in result["observed"].items():
        if not observations:
            continue
        # Observations were appended from strongest to weakest source.
        selected = observations[0]
        result["identity"][field] = selected["value"]
        result["provenance"][field] = selected["source"]
        for alternative in observations[1:]:
            if alternative["value"] != selected["value"]:
                result["conflicts"].append(
                    {
                        "field": field,
                        "selected": selected,
                        "conflicting": alternative,
                    }
                )


def _client_kind(headers, body):
    metadata = _object(body.get("client_metadata"))
    if (
        "x-codex-turn-metadata" in metadata
        or "thread_id" in metadata
        or "x-codex-turn-metadata" in headers
        or "x-codex-parent-thread-id" in headers
        or ("session-id" in headers and "codex" in str(headers.get("user-agent", "")).lower())
    ):
        return "codex"
    if (
        "x-opencode-session" in headers
        or "x-session-affinity" in headers
        or ("x-session-id" in headers and "opencode" in str(headers.get("user-agent", "")).lower())
    ):
        return "opencode"
    metadata = _object(body.get("metadata"))
    if "user_id" in metadata or "user_id" in body or "x-claude-code-session-id" in headers or "x-claude-code-agent-id" in headers:
        return "claude"
    return "unknown"


def _anthropic_nodes(body):
    nodes = []
    nodes.extend(_list_of_objects(body.get("content")))
    for message in _list_of_objects(body.get("messages")):
        nodes.extend(_list_of_objects(message.get("content")))
    return nodes


def _responses_nodes(body):
    nodes = []
    nodes.extend(_list_of_objects(body.get("input")))
    nodes.extend(_list_of_objects(body.get("output")))
    response = _object(body.get("response"))
    nodes.extend(_list_of_objects(response.get("output")))
    return nodes


def _chat_messages(body):
    messages = _list_of_objects(body.get("messages"))
    messages.extend(_list_of_objects(body.get("choices"), key="message"))
    message = _object(body.get("message"))
    if message:
        messages.append(message)
    return messages


def _tool_record(call_id, protocol):
    return {"call_id": call_id, "protocol": protocol}


def _headers(headers):
    if not isinstance(headers, dict):
        return {}
    return {str(key).lower(): value for key, value in headers.items()}


def _object(value):
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError:
            return {}
        return parsed if isinstance(parsed, dict) else {}
    return {}


def _list_of_objects(value, key=None):
    if not isinstance(value, list):
        return []
    if key is None:
        return [item for item in value if isinstance(item, dict)]
    return [item[key] for item in value if isinstance(item, dict) and isinstance(item.get(key), dict)]


def _text(value):
    return isinstance(value, str) and value != ""


def _claude_session_id(value):
    """Extract only known Claude Code user-ID encodings.

    ``metadata.user_id`` is currently a JSON string containing all three fields.
    The legacy value is a ``user_<device>_session_<uuid>`` identifier. Unknown
    values are deliberately rejected rather than promoted to session IDs.
    """
    structured = _object(value)
    if _text(structured.get("session_id")) and all(isinstance(structured.get(key), str) for key in ("device_id", "account_uuid")):
        return structured["session_id"]
    if not _text(value) or not value.startswith("user_") or value.count("_session_") != 1:
        return None
    device, session_uuid = value[len("user_") :].split("_session_", 1)
    if not device:
        return None
    try:
        return str(uuid.UUID(session_uuid))
    except ValueError:
        return None
