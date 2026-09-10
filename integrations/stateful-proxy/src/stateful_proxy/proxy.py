"""Loopback-only HTTP capture and tool-call-ID rewrite proof of concept.

By default this proxy is correlation plumbing, not APPA protection. Its
optional --appa-enforce mode mediates only root trajectories with native
session/thread assertions; it does not protect child, fork, or compaction
lifecycles and fails closed for those unsupported cases.

The optional /inject/anthropic route emits a visible prompt marker as an
untrusted correlation hint only. It can be copied or lost and does not provide
protected lineage, identity, authorization, or evidence of parentage.

Credentials are loaded only by this relay process from a restricted JSON file.
Captures redact common sensitive JSON fields and never retain authorization,
key, or cookie headers.
"""

from __future__ import annotations

import argparse
import copy
import http.client
import importlib
import json
import os
import re
import secrets
import sqlite3
import stat
import threading
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

from .appa_gate import Gate, GateError
from .checkpoint_client import CheckpointClient, CheckpointError
from .correlation import normalize_identity
from .lifecycle_ledger import LifecycleLedger, LifecycleLedgerError
from .provider_history import UnsupportedHistory, canonical_request_history, canonical_response_items, digest_prefix
from .rewritten_arguments import (
    SpawnArgumentRewriteError,
    is_spawn_tool,
    rewrite_response_arguments,
    rewrite_spawn_args,
    rewrite_sse_arguments,
)


PROVIDERS = {
    "anthropic": ("ANTHROPIC_API_KEY", "x-api-key"),
    "openai": ("OPENAI_API_KEY", "authorization"),
    "kimi": ("KIMI_API_KEY", "authorization"),
}
KEY_FIELDS = frozenset(key_name for key_name, _ in PROVIDERS.values())
ALLOWLISTED_HEADERS = {
    "x-request-id",
    "x-correlation-id",
    "x-session-id",
    "x-trace-id",
    "traceparent",
    "tracestate",
    "session-id", "x-session-affinity", "x-parent-session-id", "user-agent",
    "x-codex-window-id", "x-codex-turn-metadata", "x-codex-parent-thread-id",
    "x-openai-subagent", "x-opencode-project", "x-opencode-session",
    "x-opencode-request", "x-opencode-client", "x-client-request-id",
    "x-claude-code-agent-id", "x-claude-code-session-id",
}
SENSITIVE_HEADER_MARKERS = ("authorization", "api-key", "apikey", "cookie", "token", "secret")
INTERNAL_LIFECYCLE_HEADERS = {"x-appa-spawn-marker", "x-appa-context-anchor"}
SENSITIVE_FIELD_MARKERS = ("auth", "api_key", "apikey", "cookie", "token", "secret", "password")
SENSITIVE_FIELD_EXACT = {"key", "authorization", "x_api_key"}
INJECTABLE_ANTHROPIC_TOOLS = {"Agent", "Task"}
MARKER_PATTERN = re.compile(
    r'<appa-correlation parent_call="(?P<parent_call>(?:call_|toolu_|tool_)[A-Za-z0-9_-]{1,128})"(?: spawn_marker="(?P<spawn_marker>spm_[A-Za-z0-9_-]+)")?/>'
)
CONTEXT_MARKER_PATTERN = re.compile(r'<appa-context anchor="(?P<anchor>v1\.[^"]+)"/>')


class MappingStore:
    """Persistent, provider-scoped bijection for tool-call identifiers."""

    def __init__(self, path: str | Path, token_factory: Callable[[], str] | None = None):
        self.path = Path(path)
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(self.path.parent, 0o700)
        self.token_factory = token_factory or (lambda: "tool_" + secrets.token_urlsafe(18))
        self._lock = threading.Lock()
        self._connection = sqlite3.connect(self.path, check_same_thread=False)
        os.chmod(self.path, 0o600)
        self._connection.execute(
            """
            CREATE TABLE IF NOT EXISTS tool_id_mappings (
                provider TEXT NOT NULL,
                original_id TEXT NOT NULL,
                replacement_id TEXT NOT NULL,
                PRIMARY KEY (provider, original_id),
                UNIQUE (provider, replacement_id)
            )
            """
        )
        self._connection.commit()

    def close(self) -> None:
        self._connection.close()

    def replacement_for(self, provider: str, original_id: str) -> str:
        with self._lock:
            row = self._connection.execute(
                "SELECT replacement_id FROM tool_id_mappings WHERE provider = ? AND original_id = ?",
                (provider, original_id),
            ).fetchone()
            if row:
                return row[0]
            while True:
                replacement = self.token_factory()
                try:
                    self._connection.execute(
                        "INSERT INTO tool_id_mappings(provider, original_id, replacement_id) VALUES (?, ?, ?)",
                        (provider, original_id, replacement),
                    )
                    self._connection.commit()
                    return replacement
                except sqlite3.IntegrityError:
                    # A rare generated-ID collision is retried; an existing original is re-read.
                    row = self._connection.execute(
                        "SELECT replacement_id FROM tool_id_mappings WHERE provider = ? AND original_id = ?",
                        (provider, original_id),
                    ).fetchone()
                    if row:
                        return row[0]

    def original_for(self, provider: str, replacement_id: str) -> str | None:
        with self._lock:
            row = self._connection.execute(
                "SELECT original_id FROM tool_id_mappings WHERE provider = ? AND replacement_id = ?",
                (provider, replacement_id),
            ).fetchone()
            return row[0] if row else None


def _replace_outbound(store: MappingStore, provider: str, value: str) -> str:
    return store.replacement_for(provider, value)


def _restore_inbound(store: MappingStore, provider: str, value: str) -> str:
    return store.original_for(provider, value) or value


def rewrite_payload(payload: Any, provider: str, direction: str, store: MappingStore) -> Any:
    """Rewrite only documented provider tool identifiers, never message IDs."""
    result = copy.deepcopy(payload)
    transform = _replace_outbound if direction == "outbound" else _restore_inbound

    def walk(item: Any) -> None:
        if isinstance(item, list):
            for child in item:
                walk(child)
            return
        if not isinstance(item, dict):
            return

        if provider == "anthropic":
            if item.get("type") == "tool_use" and isinstance(item.get("id"), str):
                item["id"] = transform(store, provider, item["id"])
            if direction == "inbound" and item.get("type") == "tool_result" and isinstance(item.get("tool_use_id"), str):
                item["tool_use_id"] = transform(store, provider, item["tool_use_id"])
        else:
            if item.get("type") in {"function_call", "function_call_output", "custom_tool_call", "custom_tool_call_output"} and isinstance(item.get("call_id"), str):
                item["call_id"] = transform(store, provider, item["call_id"])
            if direction == "inbound" and isinstance(item.get("tool_call_id"), str):
                item["tool_call_id"] = transform(store, provider, item["tool_call_id"])
            tool_calls = item.get("tool_calls")
            if isinstance(tool_calls, list):
                for tool_call in tool_calls:
                    if isinstance(tool_call, dict) and isinstance(tool_call.get("id"), str):
                        tool_call["id"] = transform(store, provider, tool_call["id"])

        # Descend through protocol containers, never arbitrary tool arguments/results.
        if item.get("type") in {"tool_use", "tool_result", "function_call", "function_call_output", "custom_tool_call", "custom_tool_call_output"}:
            return
        for key in ("messages", "input", "output", "content", "content_block", "message", "item", "response", "choices", "delta"):
            if key in item:
                walk(item[key])

    walk(result)
    return result


def correlation_marker(parent_call: str, spawn_marker: str | None = None) -> str:
    """Return an untrusted, machine-readable hint for a spawned child prompt."""
    if spawn_marker is None:
        return f'<appa-correlation parent_call="{parent_call}"/>'
    return f'<appa-correlation parent_call="{parent_call}" spawn_marker="{spawn_marker}"/>'


def inject_anthropic_correlation(
    payload: Any,
    store: MappingStore,
    spawn_marker_for: Callable[[str], str | None] | None = None,
) -> Any:
    """Prefix Agent/Task prompts after their tool-use IDs have already been rewritten.

    The marker is deliberately only visible prompt text. It confers no authority,
    can be copied or lost by a model, and must not be treated as protected lineage.
    """
    result = copy.deepcopy(payload)

    def walk(item: Any) -> None:
        if isinstance(item, list):
            for child in item:
                walk(child)
            return
        if not isinstance(item, dict):
            return
        if item.get("type") == "tool_use":
            tool_input = item.get("input")
            tool_id = item.get("id")
            if (
                item.get("name") in INJECTABLE_ANTHROPIC_TOOLS
                and isinstance(tool_id, str)
                and store.original_for("anthropic", tool_id) is not None
                and isinstance(tool_input, dict)
                and isinstance(tool_input.get("prompt"), str)
            ):
                marker = correlation_marker(tool_id, spawn_marker_for(tool_id) if spawn_marker_for else None)
                if not tool_input["prompt"].startswith(marker):
                    tool_input["prompt"] = marker + tool_input["prompt"]
            return
        for key in ("messages", "input", "output", "content", "content_block", "message", "item", "response", "choices", "delta"):
            if key in item:
                walk(item[key])

    walk(result)
    return result


def inject_spawn_carrier(payload: Any, provider: str, marker_for: Callable[[str], str | None]) -> Any:
    """Carry a proxy-issued spawn marker in documented child-launch arguments.

    The original provider arguments are admitted before this transformation and
    remain the values retained by Gate for replay/resume. The carrier is solely
    bootstrap data for the client-created child.
    """
    return rewrite_response_arguments(payload, provider, marker_for)


def spawn_carrier_from_payload(payload: Any) -> str | None:
    """Read one carrier only from ordinary user text parts at part start."""
    if not isinstance(payload, dict):
        return None
    candidates: list[str] = []
    for message in payload.get("messages", []) if isinstance(payload.get("messages"), list) else []:
        if isinstance(message, dict) and message.get("role") == "user":
            content = message.get("content")
            if isinstance(content, str):
                candidates.append(content)
            elif isinstance(content, list):
                for part in content:
                    if isinstance(part, dict) and part.get("type") in {"text", "input_text"} and isinstance(part.get("text"), str):
                        candidates.append(part["text"])
    for item in payload.get("input", []) if not candidates and isinstance(payload.get("input"), list) else []:
        if isinstance(item, dict) and item.get("role") == "user":
            content = item.get("content")
            if isinstance(content, str):
                candidates.append(content)
            elif isinstance(content, list):
                for part in content:
                    if isinstance(part, dict) and part.get("type") in {"text", "input_text"} and isinstance(part.get("text"), str):
                        candidates.append(part["text"])
    markers = [match.group("spawn_marker") for text in candidates if (match := MARKER_PATTERN.match(text)) and match.group("spawn_marker")]
    if len(markers) > 1:
        raise GateMediationError("child bootstrap contains multiple carrier candidates")
    return markers[0] if markers else None


def context_anchor_from_payload(payload: Any) -> str | None:
    """No request text is an anchor authority without a registered checkpoint chain.

    A valid signature proves only that the proxy issued a token at some point;
    it does not prove the token is at the required provider response position,
    belongs to this context prefix, or names the current policy state. The
    checkpoint worker must register an opaque response-item digest before this
    function can safely return an anchor.
    """
    return None


def stamp_context_carrier(payload: Any, provider: str, anchor: str) -> Any:
    """Attach the signed anchor to provider model context without changing policy data."""
    result = copy.deepcopy(payload)
    carrier = f'<appa-context anchor="{anchor}"/>'
    if provider == "anthropic" and isinstance(result, dict) and isinstance(result.get("content"), list):
        result["content"].append({"type": "text", "text": carrier})
    elif provider != "anthropic" and isinstance(result, dict) and isinstance(result.get("output"), list):
        result["output"].append({"type": "output_text", "text": carrier})
    return result


def detect_injected_markers(body: bytes, store: MappingStore) -> list[dict[str, Any]]:
    """Report markers in user message text without using them for authorization."""
    try:
        payload = json.loads(body)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return []
    if not isinstance(payload, dict) or not isinstance(payload.get("messages"), list):
        return []
    markers: list[dict[str, Any]] = []
    for message_index, message in enumerate(payload["messages"]):
        if not isinstance(message, dict) or message.get("role") != "user":
            continue
        content = message.get("content")
        text_items: list[tuple[str, str]] = []
        if isinstance(content, str):
            text_items.append((f"messages[{message_index}].content", content))
        elif isinstance(content, list):
            for content_index, block in enumerate(content):
                if isinstance(block, dict) and block.get("type") == "text" and isinstance(block.get("text"), str):
                    text_items.append((f"messages[{message_index}].content[{content_index}].text", block["text"]))
        for path, text in text_items:
            for match in MARKER_PATTERN.finditer(text):
                parent_call = match.group("parent_call")
                markers.append({
                    "path": path,
                    "message_index": message_index,
                    "parent_call": parent_call,
                    "recognized_mapping": store.original_for("anthropic", parent_call) is not None,
                })
    return markers


class SSETransformer:
    """Buffers complete SSE events so JSON may arrive across arbitrary chunks."""

    def __init__(self, provider: str, direction: str | None, store: MappingStore, on_data: Callable[[Any], None] | None = None):
        self.provider = provider
        self.direction = direction
        self.store = store
        self.on_data = on_data
        self._line_buffer = b""
        self._event_lines: list[bytes] = []

    def feed(self, chunk: bytes) -> bytes:
        self._line_buffer += chunk
        output = bytearray()
        while b"\n" in self._line_buffer:
            line, self._line_buffer = self._line_buffer.split(b"\n", 1)
            line += b"\n"
            if line in {b"\n", b"\r\n"}:
                output.extend(self._finish_event())
                output.extend(line)
            else:
                self._event_lines.append(line)
        return bytes(output)

    def finish(self) -> bytes:
        output = bytearray()
        if self._line_buffer:
            self._event_lines.append(self._line_buffer)
            self._line_buffer = b""
        output.extend(self._finish_event())
        return bytes(output)

    def _finish_event(self) -> bytes:
        lines = self._event_lines
        self._event_lines = []
        if not lines:
            return b""
        data_indexes = [index for index, line in enumerate(lines) if line.lstrip().startswith(b"data:")]
        if len(data_indexes) != 1:
            return b"".join(lines)
        index = data_indexes[0]
        line = lines[index]
        prefix, raw_data = line.split(b":", 1)
        try:
            value = json.loads(raw_data.strip())
        except (UnicodeDecodeError, json.JSONDecodeError):
            return b"".join(lines)
        rewritten = rewrite_payload(value, self.provider, self.direction, self.store) if self.direction else value
        if self.on_data:
            self.on_data(rewritten)
        ending = b"\r\n" if line.endswith(b"\r\n") else b"\n"
        lines[index] = prefix + b": " + json.dumps(rewritten, separators=(",", ":")).encode("utf-8") + ending
        return b"".join(lines)


def _decode_sse_event(lines: list[bytes]) -> tuple[int, Any] | None:
    data_indexes = [index for index, line in enumerate(lines) if line.lstrip().startswith(b"data:")]
    if len(data_indexes) != 1:
        return None
    index = data_indexes[0]
    try:
        return index, json.loads(lines[index].split(b":", 1)[1].strip())
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None


def _replace_sse_data(lines: list[bytes], index: int, value: Any) -> bytes:
    ending = b"\r\n" if lines[index].endswith(b"\r\n") else b"\n"
    prefix = lines[index].split(b":", 1)[0]
    replaced = list(lines)
    replaced[index] = prefix + b": " + json.dumps(value, separators=(",", ":")).encode("utf-8") + ending
    return b"".join(replaced)


def _encode_anthropic_sse_event(event_type: str, value: dict[str, Any], terminated: bool = True) -> bytes:
    ending = b"\n\n" if terminated else b"\n"
    return b"event: " + event_type.encode("utf-8") + b"\ndata: " + json.dumps(value, separators=(",", ":")).encode("utf-8") + ending


class AnthropicInjectionSSETransformer:
    """Buffer Agent/Task argument deltas, then emit one complete injected JSON block."""

    def __init__(
        self,
        store: MappingStore,
        on_data: Callable[[Any], None] | None = None,
        spawn_marker_for: Callable[[str], str | None] | None = None,
    ):
        self.store = store
        self.on_data = on_data
        self.spawn_marker_for = spawn_marker_for
        self._line_buffer = b""
        self._event_lines: list[bytes] = []
        self._pending: dict[int, dict[str, Any]] = {}
        self._suppress_delimiter = False

    def feed(self, chunk: bytes) -> bytes:
        self._line_buffer += chunk
        output = bytearray()
        while b"\n" in self._line_buffer:
            line, self._line_buffer = self._line_buffer.split(b"\n", 1)
            line += b"\n"
            if line in {b"\n", b"\r\n"}:
                self._suppress_delimiter = False
                output.extend(self._finish_event())
                if not self._suppress_delimiter:
                    output.extend(line)
            else:
                self._event_lines.append(line)
        return bytes(output)

    def finish(self) -> bytes:
        if self._line_buffer:
            self._event_lines.append(self._line_buffer)
            self._line_buffer = b""
        output = bytearray(self._finish_event())
        for index in list(self._pending):
            output.extend(self._flush_original(index))
        return bytes(output)

    def _finish_event(self) -> bytes:
        lines = self._event_lines
        self._event_lines = []
        if not lines:
            return b""
        decoded = _decode_sse_event(lines)
        if decoded is None:
            return b"".join(lines)
        data_index, value = decoded
        rewritten = rewrite_payload(value, "anthropic", "outbound", self.store)
        event_type = rewritten.get("type") if isinstance(rewritten, dict) else None
        index = rewritten.get("index") if isinstance(rewritten, dict) else None

        if event_type == "content_block_start" and isinstance(index, int):
            content_block = rewritten.get("content_block")
            if (
                isinstance(content_block, dict)
                and content_block.get("type") == "tool_use"
                and content_block.get("name") in INJECTABLE_ANTHROPIC_TOOLS
                and isinstance(content_block.get("id"), str)
            ):
                output = self._flush_original(index) if index in self._pending else b""
                self._pending[index] = {"events": [(lines, data_index, rewritten)], "fragments": [], "unsupported": False}
                self._suppress_delimiter = True
                return output

        if isinstance(index, int) and index in self._pending:
            pending = self._pending[index]
            pending["events"].append((lines, data_index, rewritten))
            if event_type == "content_block_delta":
                delta = rewritten.get("delta")
                if isinstance(delta, dict) and delta.get("type") == "input_json_delta" and isinstance(delta.get("partial_json"), str):
                    pending["fragments"].append(delta["partial_json"])
                else:
                    pending["unsupported"] = True
                self._suppress_delimiter = True
                return b""
            if event_type == "content_block_stop":
                return self._emit_injected(index)
            pending["unsupported"] = True
            return b""

        self._record(rewritten)
        return _replace_sse_data(lines, data_index, rewritten)

    def _emit_injected(self, index: int) -> bytes:
        pending = self._pending.pop(index)
        if pending["unsupported"]:
            return self._render_original(pending["events"])
        try:
            tool_input = json.loads("".join(pending["fragments"]))
        except json.JSONDecodeError:
            return self._render_original(pending["events"])
        start = pending["events"][0][2]
        content_block = start["content_block"]
        prompt = tool_input.get("prompt") if isinstance(tool_input, dict) else None
        if not isinstance(prompt, str):
            return self._render_original(pending["events"])
        marker = correlation_marker(
            content_block["id"],
            self.spawn_marker_for(content_block["id"]) if self.spawn_marker_for else None,
        )
        if not prompt.startswith(marker):
            tool_input["prompt"] = marker + prompt
        start = copy.deepcopy(start)
        start["content_block"]["input"] = {}
        delta = {
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "input_json_delta", "partial_json": json.dumps(tool_input, separators=(",", ":"))},
        }
        stop = {"type": "content_block_stop", "index": index}
        self._record(start)
        self._record(delta)
        self._record(stop)
        return (
            _encode_anthropic_sse_event("content_block_start", start)
            + _encode_anthropic_sse_event("content_block_delta", delta)
            + _encode_anthropic_sse_event("content_block_stop", stop, terminated=False)
        )

    def _flush_original(self, index: int) -> bytes:
        return self._render_original(self._pending.pop(index)["events"])

    def _render_original(self, events: list[tuple[list[bytes], int, Any]]) -> bytes:
        output = bytearray()
        for lines, data_index, value in events:
            self._record(value)
            output.extend(_replace_sse_data(lines, data_index, value))
        return bytes(output)

    def _record(self, value: Any) -> None:
        if self.on_data:
            self.on_data(value)


def _is_sensitive_name(name: str) -> bool:
    normalized = name.lower().replace("-", "_")
    return normalized in SENSITIVE_FIELD_EXACT or any(marker in normalized for marker in SENSITIVE_FIELD_MARKERS)


def redact(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: "[REDACTED]" if _is_sensitive_name(str(key)) else redact(child) for key, child in value.items()}
    if isinstance(value, list):
        return [redact(item) for item in value]
    return value


def safe_body(body: bytes) -> Any:
    try:
        return redact(json.loads(body))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return {"non_json_bytes": len(body)}


def safe_headers(headers: Any) -> dict[str, str]:
    return {name.lower(): value for name, value in headers.items() if name.lower() in ALLOWLISTED_HEADERS}


class CaptureLogger:
    def __init__(self, directory: str | Path):
        self.directory = Path(directory)
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(self.directory, 0o700)
        self.path = self.directory / "capture.jsonl"
        self._lock = threading.Lock()

    def write(self, record: dict[str, Any]) -> None:
        record = {"at": datetime.now(timezone.utc).isoformat(), **record}
        line = json.dumps(redact(record), separators=(",", ":"), sort_keys=True) + "\n"
        with self._lock:
            with self.path.open("a", encoding="utf-8") as handle:
                os.chmod(self.path, 0o600)
                handle.write(line)


class GateMediationError(RuntimeError):
    """An unenforceable lifecycle edge. Callers must never forward it."""

    def __init__(self, message: str, *, remedy_available: bool = False, code: str = "APPA_ENFORCEMENT_REFUSED"):
        super().__init__(message)
        self.remedy_available = remedy_available
        self.code = code


def gate_refusal_body(error: GateMediationError) -> bytes:
    """Render no runtime, tool, argument, or child-result detail to clients."""
    messages = {
        "APPA_CHILD_TOOL_BLOCKED": "A child tool operation was blocked by policy.",
        "APPA_CHILD_RETURN_BLOCKED": "A child return was blocked by policy.",
        "APPA_RUNTIME_UNAVAILABLE": "Policy enforcement is temporarily unavailable.",
    }
    payload: dict[str, Any] = {
        "error": {
            "type": "appa_enforcement",
            "code": error.code,
            "message": messages.get(error.code, "Request was refused by policy enforcement."),
        }
    }
    if error.remedy_available:
        payload["error"]["remedy"] = "A policy remedy may be available through the configured review path."
    return json.dumps(payload, separators=(",", ":")).encode("utf-8")


@dataclass(frozen=True)
class RootContext:
    root_id: str
    client: str
    evidence: dict[str, Any]


@dataclass(frozen=True)
class ToolCall:
    call_id: str
    name: str
    args: Any


@dataclass
class TrackedCall:
    name: str
    args_fingerprint: str
    result_fingerprint: str | None = None
    delivered_result: Any = None


class GateTrace:
    """Redacted, private proxy-side trace of every gate request and decision."""

    def __init__(self, path: str | Path):
        self.path = Path(path)
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(self.path.parent, 0o700)
        self._lock = threading.Lock()

    def write(self, record: dict[str, Any]) -> None:
        line = json.dumps(
            redact({"at": datetime.now(timezone.utc).isoformat(), **record}),
            separators=(",", ":"),
            sort_keys=True,
        ) + "\n"
        with self._lock:
            with self.path.open("a", encoding="utf-8") as handle:
                os.chmod(self.path, 0o600)
                handle.write(line)


def _canonical_json(value: Any) -> str:
    try:
        return json.dumps(value, separators=(",", ":"), ensure_ascii=True, allow_nan=False)
    except (TypeError, ValueError) as error:
        raise GateMediationError("tool arguments or result are not JSON-safe") from error


def _header_value(headers: Any, name: str) -> str:
    for key, value in headers.items():
        if str(key).lower() == name:
            return value.strip() if isinstance(value, str) else ""
    return ""


def enforcement_root_context(headers: Any, payload: Any) -> RootContext:
    """Accept root session/thread assertions only; never manufacture a lineage."""
    claude_agent_id = _header_value(headers, "x-claude-code-agent-id")
    if claude_agent_id:
        raise GateMediationError("root-only APPA enforcement rejects Claude child requests with an agent ID")
    evidence = normalize_identity({str(key).lower(): value for key, value in headers.items()}, payload)
    identity = evidence["identity"]
    if evidence["conflicts"]:
        raise GateMediationError("root-only APPA enforcement rejects conflicting native identity assertions")
    unsupported = (
        identity.get("agent_id"),
        identity.get("parent_session_id"),
        identity.get("parent_thread_id"),
        identity.get("forked_from_thread_id"),
        identity.get("compaction"),
    )
    if any(value not in (None, "", False) for value in unsupported):
        raise GateMediationError("child, fork, or compaction identity is not protected by this root-only relay")
    thread_id = identity.get("thread_id")
    session_id = identity.get("session_id")
    # Claude's observed root header is only fallback evidence; no agent ID means root.
    if not isinstance(session_id, str) or not session_id:
        session_id = _header_value(headers, "x-claude-code-session-id")
    if isinstance(thread_id, str) and thread_id:
        return RootContext(f"thread:{thread_id}", evidence["client"], evidence)
    if isinstance(session_id, str) and session_id:
        return RootContext(f"session:{session_id}", evidence["client"], evidence)
    raise GateMediationError("APPA enforcement requires a native client session or thread identity")


def _tool_call(call_id: Any, name: Any, args: Any) -> ToolCall:
    if not isinstance(call_id, str) or not call_id or not isinstance(name, str) or not name:
        raise GateMediationError("provider response contains an unsupported tool call shape")
    _canonical_json(args)
    return ToolCall(call_id, name, args)


def _qualified_tool_name(item: dict[str, Any]) -> Any:
    """Preserve a Responses namespace as part of the Gate tool identity."""
    name = item.get("name")
    namespace = item.get("namespace")
    if not isinstance(namespace, str) or not namespace:
        return name
    if not isinstance(name, str) or not name:
        return name
    # Codex exposes MCP calls as namespace=mcp__server, name=tool. Preserve
    # both pieces in the verified policy-facing dotted identity.
    return f"{namespace}.{name}" if namespace.startswith("mcp__") else f"{namespace}:{name}"


def _json_arguments(value: Any) -> Any:
    if not isinstance(value, str):
        raise GateMediationError("provider function-call arguments are not a JSON string")
    try:
        return json.loads(value)
    except json.JSONDecodeError as error:
        raise GateMediationError("provider function-call arguments are incomplete or invalid") from error


def canonical_child_return(body: Any) -> str:
    """Render the captured Anthropic all-text result form for the Gate only."""
    if isinstance(body, str):
        return body
    if not isinstance(body, list):
        raise GateMediationError("child return has an unsupported representation", code="APPA_CHILD_RETURN_BLOCKED")
    texts: list[str] = []
    for block in body:
        if not isinstance(block, dict) or block.get("type") != "text" or not isinstance(block.get("text"), str):
            raise GateMediationError("child return contains a non-text or malformed block", code="APPA_CHILD_RETURN_BLOCKED")
        texts.append(block["text"])
    return "\n".join(texts)


_CLAUDE_ASYNC_RECEIPT = re.compile(
    r"\AAsync agent launched successfully\..*?\nagentId: (?P<agent_id>[A-Za-z0-9_-]+) \(internal ID.*\Z",
    re.DOTALL,
)
_WAIT_ALIASES = {"multi_agent_v1:wait_agent", "agents:wait_agent"}


def wait_targets(args: Any) -> list[str]:
    if not isinstance(args, dict) or set(args) - {"targets", "timeout_ms"} or "targets" not in args:
        raise GateMediationError("wait control has unsupported arguments")
    targets = args["targets"]
    targets = [targets] if isinstance(targets, str) else targets
    if not isinstance(targets, list) or not targets or any(not isinstance(target, str) or not target for target in targets):
        raise GateMediationError("wait control requires nonempty child targets")
    timeout = args.get("timeout_ms")
    if timeout is None:
        timeout = 30000  # Codex DEFAULT_WAIT_TIMEOUT_MS
    if isinstance(timeout, bool) or not isinstance(timeout, int) or timeout < 0 or timeout > 3_600_000:
        raise GateMediationError("wait control timeout is outside the documented safe limit")
    return targets


def async_spawn_receipt(provider: str, name: str, body: Any) -> str | None:
    """Recognize only documented async spawn receipts, never arbitrary tool data."""
    if not is_spawn_tool(name):
        return None
    if provider == "openai":
        value = body
        if isinstance(value, str):
            try:
                value = json.loads(value)
            except json.JSONDecodeError:
                return None
        if not isinstance(value, dict) or not {"agent_id"} <= set(value) or set(value) - {"agent_id", "nickname"}:
            return None
        agent_id = value.get("agent_id")
        nickname = value.get("nickname")
        if not isinstance(agent_id, str) or not agent_id:
            return None
        if nickname is not None and (not isinstance(nickname, str) or len(nickname) > 128):
            return None
        if isinstance(value, dict):
            # nickname is display-only and never participates in binding.
            return agent_id
        return None
    if provider == "anthropic" and isinstance(body, list) and len(body) == 1:
        block = body[0]
        if isinstance(block, dict) and block.get("type") == "text" and isinstance(block.get("text"), str):
            match = _CLAUDE_ASYNC_RECEIPT.fullmatch(block["text"])
            return match.group("agent_id") if match else None
    return None


def _value_shape(value: Any) -> dict[str, Any]:
    """Trace only structure when a native async receipt cannot be recognized."""
    if isinstance(value, dict):
        return {"type": "object", "keys": sorted(str(key) for key in value)}
    if isinstance(value, list):
        return {
            "type": "array",
            "length": len(value),
            "item_types": [item.get("type") if isinstance(item, dict) else type(item).__name__ for item in value],
        }
    if isinstance(value, str):
        try:
            decoded = json.loads(value)
        except json.JSONDecodeError:
            decoded = None
        return {
            "type": "str",
            "length": len(value),
            "json_object_keys": sorted(str(key) for key in decoded) if isinstance(decoded, dict) else None,
        }
    return {"type": type(value).__name__}


def response_tool_calls(provider: str, payload: Any) -> list[ToolCall]:
    """Extract only documented complete, non-streaming tool call shapes."""
    if not isinstance(payload, dict):
        return []
    calls: list[ToolCall] = []
    if provider == "anthropic":
        for block in payload.get("content", []):
            if isinstance(block, dict) and block.get("type") == "tool_use":
                if not isinstance(block.get("input"), dict):
                    raise GateMediationError("Anthropic tool call has no complete object input")
                calls.append(_tool_call(block.get("id"), block.get("name"), block["input"]))
        return calls

    for item in payload.get("output", []):
        if not isinstance(item, dict):
            continue
        if item.get("type") == "custom_tool_call":
            raise GateMediationError("custom tool calls are unsupported and denied without fabricating input")
        if item.get("type") == "function_call":
            calls.append(_tool_call(item.get("call_id"), _qualified_tool_name(item), _json_arguments(item.get("arguments"))))
    for choice in payload.get("choices", []):
        message = choice.get("message") if isinstance(choice, dict) else None
        for tool_call in message.get("tool_calls", []) if isinstance(message, dict) else []:
            function = tool_call.get("function") if isinstance(tool_call, dict) else None
            if not isinstance(function, dict):
                raise GateMediationError("chat-completions tool call has no function payload")
            calls.append(_tool_call(tool_call.get("id"), function.get("name"), _json_arguments(function.get("arguments"))))
    return calls


def _sse_values(raw: bytes) -> list[dict[str, Any]]:
    values: list[dict[str, Any]] = []
    for event in re.split(br"\r?\n\r?\n", raw):
        data = [line.split(b":", 1)[1].strip() for line in event.splitlines() if line.lstrip().startswith(b"data:")]
        if len(data) != 1 or data[0] == b"[DONE]":
            continue
        try:
            value = json.loads(data[0])
        except (UnicodeDecodeError, json.JSONDecodeError):
            continue
        if isinstance(value, dict):
            values.append(value)
    return values


class ToolCallSSEBuffer:
    """Accumulate a complete provider SSE response before any tool data is released."""

    def __init__(self, provider: str):
        self.provider = provider
        self._chunks: list[bytes] = []

    def feed(self, chunk: bytes) -> None:
        self._chunks.append(chunk)

    def finish(self) -> tuple[bytes, list[ToolCall]]:
        raw = b"".join(self._chunks)
        values = _sse_values(raw)
        if self.provider == "anthropic":
            return raw, self._anthropic(values)
        return raw, self._openai(values)

    @staticmethod
    def _anthropic(values: list[dict[str, Any]]) -> list[ToolCall]:
        pending: dict[int, dict[str, Any]] = {}
        calls: list[ToolCall] = []
        for value in values:
            event_type = value.get("type")
            index = value.get("index")
            if event_type == "content_block_start" and isinstance(index, int):
                block = value.get("content_block")
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    pending[index] = {"id": block.get("id"), "name": block.get("name"), "parts": [], "input": block.get("input")}
            elif isinstance(index, int) and index in pending and event_type == "content_block_delta":
                delta = value.get("delta")
                if not isinstance(delta, dict) or delta.get("type") != "input_json_delta" or not isinstance(delta.get("partial_json"), str):
                    raise GateMediationError("Anthropic tool arguments were not delivered as complete JSON deltas")
                pending[index]["parts"].append(delta["partial_json"])
            elif isinstance(index, int) and index in pending and event_type == "content_block_stop":
                call = pending.pop(index)
                if call["parts"]:
                    args = _json_arguments("".join(call["parts"]))
                elif isinstance(call["input"], dict):
                    args = call["input"]
                else:
                    raise GateMediationError("Anthropic tool call ended without complete arguments")
                calls.append(_tool_call(call["id"], call["name"], args))
        if pending:
            raise GateMediationError("Anthropic tool call stream ended before arguments completed")
        return calls

    @staticmethod
    def _openai(values: list[dict[str, Any]]) -> list[ToolCall]:
        responses: dict[str, dict[str, Any]] = {}
        chats: dict[int, dict[str, Any]] = {}
        for value in values:
            item = value.get("item")
            if isinstance(item, dict):
                if item.get("type") == "custom_tool_call":
                    raise GateMediationError("custom tool calls are unsupported and denied without fabricating input")
                if item.get("type") == "function_call":
                    call_id = item.get("call_id")
                    if not isinstance(call_id, str) or not call_id:
                        raise GateMediationError("Responses function call has no call ID")
                    state = responses.setdefault(call_id, {"name": _qualified_tool_name(item), "arguments": None})
                    state["name"] = _qualified_tool_name(item) or state["name"]
                    if isinstance(item.get("arguments"), str):
                        state["arguments"] = item["arguments"]
            if value.get("type") == "response.function_call_arguments.done":
                call_id = value.get("call_id")
                if isinstance(call_id, str) and call_id in responses and isinstance(value.get("arguments"), str):
                    responses[call_id]["arguments"] = value["arguments"]
            for choice in value.get("choices", []):
                delta = choice.get("delta") if isinstance(choice, dict) else None
                for tool_call in delta.get("tool_calls", []) if isinstance(delta, dict) else []:
                    if not isinstance(tool_call, dict) or not isinstance(tool_call.get("index"), int):
                        raise GateMediationError("chat-completions tool call stream has no index")
                    state = chats.setdefault(tool_call["index"], {"id": None, "name": None, "parts": []})
                    state["id"] = tool_call.get("id") or state["id"]
                    function = tool_call.get("function")
                    if isinstance(function, dict):
                        state["name"] = function.get("name") or state["name"]
                        if isinstance(function.get("arguments"), str):
                            state["parts"].append(function["arguments"])
        calls = [_tool_call(call_id, state["name"], _json_arguments(state["arguments"])) for call_id, state in responses.items()]
        calls.extend(_tool_call(state["id"], state["name"], _json_arguments("".join(state["parts"]))) for state in chats.values())
        return calls


class GateMediator:
    """Root-only stateful Gate bridge; all unknown lifecycle edges fail closed."""

    def __init__(self, gate_factory: Callable[[str], Gate], trace: GateTrace):
        self.gate_factory = gate_factory
        self.trace = trace
        self._gates: dict[str, Gate] = {}
        self._calls: dict[tuple[str, str, str], TrackedCall] = {}
        self._active_turns: set[str] = set()
        self._requests: dict[str, tuple[str, list[Any], str | None]] = {}
        # The ledger survives restart; Gate objects do not. This set records only
        # adapters initialized by this process, not lifecycle authorization state.
        self._opened: set[str] = set()
        self._lock = threading.Lock()

    def root(self, headers: Any, payload: Any) -> RootContext:
        context = enforcement_root_context(headers, payload)
        with self._lock:
            if context.root_id in self._gates:
                return context
            gate = self.gate_factory(context.root_id)
            self._decide(context.root_id, "session_start", {}, gate.session_start)
            self._gates[context.root_id] = gate
        return context

    def prompt(self, context: RootContext, payload: Any) -> None:
        text = _canonical_json(redact(payload))
        self._decide(context.root_id, "prompt", {"text": text}, lambda: self._gates[context.root_id].prompt(text))
        self._active_turns.add(context.root_id)

    def end_previous_turn(self, context: RootContext) -> None:
        if context.root_id not in self._active_turns:
            return
        decision = self._decide(context.root_id, "turn_end", {}, lambda: self._gates[context.root_id].turn_end())
        if decision.name != "ack":
            raise GateMediationError("OpenAPPA returned an unsupported turn-end decision")
        self._active_turns.remove(context.root_id)

    def admit(self, provider: str, context: RootContext, calls: list[ToolCall]) -> None:
        admitted: list[ToolCall] = []
        try:
            for call in calls:
                key = (provider, context.root_id, call.call_id)
                fingerprint = _canonical_json(call.args)
                existing = self._calls.get(key)
                if existing is not None:
                    if existing.name != call.name or existing.args_fingerprint != fingerprint:
                        raise GateMediationError("provider reused a tool call ID with different native identity")
                    continue
                decision = self._decide(
                    context.root_id,
                    "tool_call",
                    {"provider": provider, "call_id": call.call_id, "name": call.name, "args": call.args},
                    lambda: self._gates[context.root_id].before_call(call.call_id, call.name, call.args),
                )
                if not decision.allowed:
                    raise GateMediationError("OpenAPPA denied the proposed tool call", remedy_available=bool(getattr(decision, "offers", ())))
                self._calls[key] = TrackedCall(call.name, fingerprint)
                admitted.append(call)
        except GateMediationError:
            for call in admitted:
                try:
                    self._decide(
                        context.root_id,
                        "tool_result",
                        {"provider": provider, "call_id": call.call_id, "error": "proxy withheld before dispatch"},
                        lambda call=call: self._gates[context.root_id].after_result(call.call_id, error="proxy withheld before dispatch"),
                    )
                except GateMediationError:
                    pass
            raise

    def accept_results(self, provider: str, context: RootContext, payload: Any) -> None:
        for call_id, body, replace in self._result_slots(provider, payload):
            key = (provider, context.root_id, call_id)
            tracked = self._calls.get(key)
            if tracked is None:
                raise GateMediationError("tool result cannot be attributed to an admitted native tool call")
            fingerprint = _canonical_json(body)
            if tracked.result_fingerprint is not None:
                if tracked.result_fingerprint != fingerprint:
                    raise GateMediationError("replayed tool result changed body for the same native call ID")
                replace(tracked.delivered_result)
                continue
            decision = self._decide(
                context.root_id,
                "tool_result",
                {"provider": provider, "call_id": call_id, "body": body},
                lambda: self._gates[context.root_id].after_result(call_id, body=body),
            )
            if decision.name == "ack":
                delivered = body
            elif decision.name == "deliver_value" and "value" in decision.payload:
                delivered = decision.payload["value"]
            elif decision.name == "replace_output" and isinstance(decision.payload.get("output"), str):
                delivered = decision.payload["output"]
            elif decision.name in {"block", "deny_call", "refuse"}:
                raise GateMediationError("OpenAPPA withheld the tool result")
            else:
                raise GateMediationError("OpenAPPA returned an unsupported result decision")
            tracked.result_fingerprint = fingerprint
            tracked.delivered_result = delivered
            replace(delivered)

    def _result_slots(self, provider: str, payload: Any) -> list[tuple[str, Any, Callable[[Any], None]]]:
        if not isinstance(payload, dict):
            return []
        slots: list[tuple[str, Any, Callable[[Any], None]]] = []
        if provider == "anthropic":
            messages = payload.get("messages", [])
            for message in messages if isinstance(messages, list) else []:
                for block in message.get("content", []) if isinstance(message, dict) and isinstance(message.get("content"), list) else []:
                    if isinstance(block, dict) and block.get("type") == "tool_result" and isinstance(block.get("tool_use_id"), str):
                        slots.append((block["tool_use_id"], block.get("content"), lambda value, block=block: block.__setitem__("content", value)))
            return slots
        for item in payload.get("input", []) if isinstance(payload.get("input"), list) else []:
            if isinstance(item, dict) and item.get("type") in {"function_call_output", "custom_tool_call_output"} and isinstance(item.get("call_id"), str):
                if "output" not in item:
                    raise GateMediationError("Responses tool result has no output body")
                slots.append((item["call_id"], item["output"], lambda value, item=item: item.__setitem__("output", value)))
        for message in payload.get("messages", []) if isinstance(payload.get("messages"), list) else []:
            if isinstance(message, dict) and message.get("role") == "tool" and isinstance(message.get("tool_call_id"), str):
                if "content" not in message:
                    raise GateMediationError("chat-completions tool result has no content body")
                slots.append((message["tool_call_id"], message["content"], lambda value, message=message: message.__setitem__("content", value)))
        return slots

    def _decide(self, root_id: str, event: str, fields: dict[str, Any], call: Callable[[], Any]) -> Any:
        self.trace.write({"direction": "request", "root_id": root_id, "event": event, **fields})
        try:
            decision = call()
        except Exception as error:
            self.trace.write({"direction": "failure", "root_id": root_id, "event": event, "error": str(error)})
            raise GateMediationError("OpenAPPA runtime is unavailable or returned an unusable decision") from error
        self.trace.write({"direction": "decision", "root_id": root_id, "event": event, "decision": decision.name, "payload": decision.payload})
        return decision


@dataclass(frozen=True)
class LifecycleContext:
    trajectory_id: str
    client: str
    principal_scope: str
    kind: str
    parent_trajectory: str | None = None
    parent_call_id: str | None = None
    checkpoint_id: str | None = None
    previous_anchor: str | None = None
    spawn_marker: str | None = None
    compaction: bool = False


def _lifecycle_value(payload: Any, key: str) -> str | None:
    if not isinstance(payload, dict):
        return None
    metadata = payload.get("client_metadata")
    canonical = metadata.get("x-codex-turn-metadata") if isinstance(metadata, dict) else None
    for source in (canonical, metadata, payload):
        value = source.get(key) if isinstance(source, dict) else None
        if isinstance(value, str) and value:
            return value
    return None


def _lifecycle_flag(payload: Any, key: str) -> bool:
    if not isinstance(payload, dict):
        return False
    metadata = payload.get("client_metadata")
    canonical = metadata.get("x-codex-turn-metadata") if isinstance(metadata, dict) else None
    values = [source.get(key) for source in (canonical, metadata, payload) if isinstance(source, dict) and key in source]
    if any(not isinstance(value, bool) for value in values):
        raise GateMediationError(f"lifecycle {key} assertion has an unsupported shape")
    if len(set(values)) > 1:
        raise GateMediationError(f"lifecycle {key} assertion conflicts across native metadata")
    return bool(values and values[0])


def _lifecycle_trajectory(client: str, identity: dict[str, Any]) -> str:
    agent_id = identity.get("agent_id")
    thread_id = identity.get("thread_id")
    session_id = identity.get("session_id")
    if client == "claude" and isinstance(agent_id, str) and agent_id:
        return f"claude:agent:{agent_id}"
    if isinstance(thread_id, str) and thread_id:
        return f"{client}:thread:{thread_id}"
    if isinstance(session_id, str) and session_id:
        return f"{client}:session:{session_id}"
    raise GateMediationError("lifecycle enforcement requires a stable native trajectory identity")


def lifecycle_context(headers: Any, payload: Any, ledger: LifecycleLedger, provider: str | None = None) -> LifecycleContext:
    """Resolve native lifecycle edges from durable provider-response evidence only."""
    lowered_headers = {str(key).lower(): value for key, value in headers.items()}
    evidence = normalize_identity(lowered_headers, payload)
    identity = evidence["identity"]
    if evidence["conflicts"] or evidence["client"] == "unknown":
        raise GateMediationError("lifecycle enforcement rejects conflicting or unknown native identity")
    client = evidence["client"]
    trajectory_id = _lifecycle_trajectory(client, identity)
    try:
        history = canonical_request_history(provider, payload) if provider else None
    except UnsupportedHistory as error:
        raise GateMediationError("native history has an unsupported or opaque provider shape") from error
    native_compaction = identity.get("compaction")
    if isinstance(native_compaction, dict):
        native_compaction = native_compaction.get("kind") == "compaction" or native_compaction.get("type") == "compaction"
    if native_compaction not in (None, False, True):
        raise GateMediationError("native compaction assertion has an unsupported shape")
    compaction = bool(native_compaction or identity.get("request_kind") == "compaction" or _lifecycle_flag(payload, "compaction"))
    parent_thread, parent_session = identity.get("parent_thread_id"), identity.get("parent_session_id")
    explicit_source = identity.get("forked_from_thread_id")
    is_child = bool(identity.get("agent_id") or parent_thread or parent_session)
    if is_child and explicit_source:
        raise GateMediationError("native request ambiguously asserts both child and fork lineage")
    existing = ledger.trajectory(trajectory_id)
    if existing and history is not None and not ledger.opaque_history_allowed(trajectory_id, provider, history):
        raise GateMediationError("native history contains an unknown opaque compaction item")
    if is_child:
        if client == "codex" and not isinstance(identity.get("parent_turn_id"), str):
            raise GateMediationError("Codex child lifecycle requires canonical parent_turn_id")
        if isinstance(parent_thread, str) and parent_thread:
            parent = f"{client}:thread:{parent_thread}"
        elif isinstance(parent_session, str) and parent_session:
            parent = f"{client}:session:{parent_session}"
        elif client == "claude" and isinstance(identity.get("session_id"), str) and identity["session_id"]:
            parent = f"claude:session:{identity['session_id']}"
        else:
            raise GateMediationError("child lineage has no explicit native parent identity")
        scope = ledger.trajectory_scope(parent)
        marker = spawn_carrier_from_payload(payload)
        if marker:
            binding = ledger.bind_child(marker, trajectory_id, client, scope)
        else:
            binding = ledger.binding_for_child(trajectory_id)
            if not binding or binding.principal_scope != scope or binding.parent_trajectory != parent:
                raise GateMediationError("child requires a proxy-issued carrier in its first user prompt")
        return LifecycleContext(trajectory_id, client, scope, "child", binding.parent_trajectory, binding.parent_call_id, None, None, marker, compaction)
    if existing:
        if existing[0] != client or existing[2] not in {"root", "fork"}:
            raise GateMediationError("native trajectory conflicts with its durable lifecycle kind")
        if existing[2] == "fork":
            source = ledger.fork_info(trajectory_id)
            if not source:
                raise GateMediationError("durable fork lacks its checkpoint binding")
            return LifecycleContext(trajectory_id, client, existing[1], "fork", source[0], source[1], source[1], compaction=compaction)
        return LifecycleContext(trajectory_id, client, existing[1], existing[2], compaction=compaction)
    scope_identity = identity.get("session_id") or identity.get("thread_id")
    if not isinstance(scope_identity, str) or not scope_identity:
        raise GateMediationError("root lifecycle requires a stable native principal scope")
    if history is None and explicit_source:
        raise GateMediationError("native fork requires an exact checkpointed provider-response binding")
    if history is not None:
        try:
            binding = ledger.matching_response_binding(provider, list(history), history.bootstrap_digest)
        except LifecycleLedgerError as error:
            raise GateMediationError(str(error)) from error
        if binding:
            declared = f"{client}:thread:{explicit_source}" if isinstance(explicit_source, str) and explicit_source else None
            if declared and declared != binding.source_trajectory:
                raise GateMediationError("Codex fork parent does not match the checkpointed provider history source")
            ledger.fork(trajectory_id, binding.source_trajectory, binding.checkpoint_id, client, binding.source_scope)
            return LifecycleContext(trajectory_id, client, binding.source_scope, "fork", binding.source_trajectory, binding.checkpoint_id, binding.checkpoint_id, compaction=compaction)
        if explicit_source or any(isinstance(item, dict) and (item.get("role") in {"assistant", "tool"} or item.get("type") in {"compaction", "reasoning", "function_call", "function_call_output"}) for item in history):
            raise GateMediationError("new native root history lacks an exact checkpointed provider-response binding")
    scope = f"{client}:scope:{scope_identity}"
    ledger.ensure_root(trajectory_id, client, scope)
    return LifecycleContext(trajectory_id, client, scope, "root", compaction=compaction)


class LifecycleGateMediator:
    """Durable lifecycle bridge using the API specified in LIFECYCLE-PROXY-NEEDS."""

    def __init__(self, gate_factory: Callable[[str], Any], trace: GateTrace, ledger: LifecycleLedger, checkpoint_client: CheckpointClient | None = None):
        self.gate_factory = gate_factory
        self.trace = trace
        self.ledger = ledger
        self.checkpoint_client = checkpoint_client
        self._gates: dict[str, Any] = {}
        self._wait_bindings: dict[tuple[str, str, str], list[SpawnBinding]] = {}
        self._receipt_children: dict[tuple[str, str], str] = {}
        self._child_started = threading.Condition()
        self._active_turns: set[str] = set()
        self._requests: dict[str, tuple[str, list[Any], str | None]] = {}
        self._opened: set[str] = set()
        self._lock = threading.Lock()

    def open(self, headers: Any, payload: Any, provider: str | None = None) -> LifecycleContext:
        try:
            context = lifecycle_context(headers, payload, self.ledger, provider)
        except LifecycleLedgerError as error:
            raise GateMediationError(str(error)) from error
        gate = self._gate(context.trajectory_id)
        if context.kind == "root":
            self._start_gate(context, gate)
        elif context.kind == "child":
            self._event(context, "child_start", {"parent": context.parent_trajectory, "call": context.parent_call_id}, lambda: gate.child_start(
                trajectory_id=context.trajectory_id,
                parent_trajectory_id=context.parent_trajectory,
                parent_call_id=context.parent_call_id,
                principal_scope=context.principal_scope,
                inherited_checkpoint=self.ledger.current_anchor(context.parent_trajectory),
            ))
            try:
                self.ledger.mark_child_started(context.trajectory_id)
            except LifecycleLedgerError as error:
                raise GateMediationError(str(error)) from error
            with self._child_started:
                self._child_started.notify_all()
        elif context.kind == "fork":
            if not self.checkpoint_client or not context.checkpoint_id:
                raise GateMediationError("native fork requires the trusted Gate checkpoint API described in LIFECYCLE-PROXY-NEEDS")
            if context.trajectory_id not in self._opened:
                try:
                    self.checkpoint_client.fork(context.checkpoint_id, context.trajectory_id)
                except CheckpointError as error:
                    raise GateMediationError("trusted checkpoint fork creation failed", code="APPA_RUNTIME_UNAVAILABLE") from error
                self._start_gate(context, gate)
        if context.compaction:
            self.trace.write({"direction": "telemetry", "trajectory_id": context.trajectory_id, "event": "compaction", "kind": context.kind})
        return context

    def _start_gate(self, context: LifecycleContext, gate: Any) -> None:
        if context.trajectory_id in self._opened:
            return
        decision = self._decide(context.trajectory_id, "session_start", {}, gate.session_start)
        if decision.name != "ack":
            raise GateMediationError("OpenAPPA rejected lifecycle event session_start")
        self._opened.add(context.trajectory_id)

    def prompt(self, context: LifecycleContext, payload: Any, provider: str | None = None) -> None:
        if provider:
            try:
                history = canonical_request_history(provider, payload)
            except UnsupportedHistory as error:
                raise GateMediationError("native history has an unsupported provider shape") from error
            if not self.ledger.opaque_history_allowed(context.trajectory_id, provider, list(history)):
                raise GateMediationError("native history contains an unknown opaque compaction item")
            self._requests[context.trajectory_id] = (provider, list(history), history.bootstrap_digest)
        text = _canonical_json(redact(payload))
        self._decide(context.trajectory_id, "prompt", {"text": text}, lambda: self._gate(context.trajectory_id).prompt(text))
        self._active_turns.add(context.trajectory_id)

    def end_previous_turn(self, context: LifecycleContext) -> None:
        if context.trajectory_id not in self._active_turns:
            return
        decision = self._decide(context.trajectory_id, "turn_end", {}, lambda: self._gate(context.trajectory_id).turn_end())
        if decision.name != "ack":
            raise GateMediationError("OpenAPPA returned an unsupported turn-end decision")
        self._active_turns.remove(context.trajectory_id)

    def register_response(self, context: LifecycleContext, provider: str, response: Any, actual_message_bytes: bytes) -> bool:
        """Checkpoint a quiescent emitted response and bind its exact canonical prefix."""
        request = self._requests.get(context.trajectory_id)
        try:
            issued = canonical_response_items(provider, response)
            self.ledger.record_observed_opaque(context.trajectory_id, provider, issued)
        except (UnsupportedHistory, ValueError):
            return False
        if context.kind not in {"root", "fork"} or not request or request[0] != provider:
            return False
        # A tool-use response remains open until its exact result is admitted.
        # It cannot be checkpointed without losing the runtime call correlation.
        if self.ledger.has_open_call(context.trajectory_id):
            return False
        self.end_previous_turn(context)
        try:
            checkpoint = self.checkpoint_client.create(context.trajectory_id) if self.checkpoint_client else None
            if checkpoint is None:
                return False
            self.ledger.record_checkpoint(context.trajectory_id, checkpoint.checkpoint_id)
            self.ledger.register_response(context.trajectory_id, provider, checkpoint.checkpoint_id, request[1], request[1] + issued, digest_prefix(issued), request[2], actual_message_bytes)
            return True
        except (CheckpointError, LifecycleLedgerError, UnsupportedHistory, ValueError) as error:
            self.trace.write({"direction": "binding_unavailable", "trajectory_id": context.trajectory_id, "provider": provider, "error": str(error)})
            return False

    def admit(self, provider: str, context: LifecycleContext, calls: list[ToolCall]) -> list[ToolCall]:
        admitted: list[ToolCall] = []
        for call in calls:
            try:
                admitted_call = call
                if call.name in _WAIT_ALIASES:
                    bindings = []
                    for target in wait_targets(call.args):
                        binding = self.ledger.binding_for_child(f"codex:thread:{target}")
                        if not binding or binding.parent_trajectory != context.trajectory_id or binding.principal_scope != context.principal_scope:
                            raise GateMediationError("wait target is not an exact child owned by this parent")
                        bindings.append(binding)
                    self._wait_bindings[(provider, context.trajectory_id, call.call_id)] = bindings
                if is_spawn_tool(call.name):
                    marker = self.ledger.prepare_spawn_marker(
                        context.trajectory_id,
                        call.call_id,
                        context.principal_scope,
                        provider,
                        call.args,
                    )
                    admitted_call = ToolCall(call.call_id, call.name, rewrite_spawn_args(call.call_id, call.name, call.args, marker))
                reservation = self.ledger.reserve_call(
                    context.trajectory_id, provider, admitted_call.call_id, admitted_call.name, admitted_call.args
                )
            except (LifecycleLedgerError, SpawnArgumentRewriteError) as error:
                raise GateMediationError(str(error)) from error
            if reservation == "replay":
                admitted.append(admitted_call)
                continue
            decision = self._decide(
                context.trajectory_id,
                "tool_call",
                {"provider": provider, "call_id": admitted_call.call_id, "name": admitted_call.name, "args": admitted_call.args},
                lambda: self._gate(context.trajectory_id).before_call(admitted_call.call_id, admitted_call.name, admitted_call.args),
            )
            if not decision.allowed:
                raise GateMediationError(
                    "OpenAPPA denied the proposed tool call",
                    remedy_available=bool(getattr(decision, "offers", ())),
                    code="APPA_CHILD_TOOL_BLOCKED" if context.kind == "child" else "APPA_ENFORCEMENT_REFUSED",
                )
            try:
                if is_spawn_tool(call.name) and not decision.spawn_binding:
                    raise GateMediationError("admitted spawn lacks a Gate-issued spawn_binding")
                if is_spawn_tool(call.name):
                    self.ledger.activate_spawn_marker(
                        context.trajectory_id,
                        call.call_id,
                        context.principal_scope,
                        provider,
                        call.args,
                        decision.spawn_binding,
                    )
                self.ledger.complete_call(context.trajectory_id, provider, call.call_id)
            except LifecycleLedgerError as error:
                raise GateMediationError(str(error)) from error
            admitted.append(admitted_call)
        return admitted

    def accept_results(self, provider: str, context: LifecycleContext, payload: Any) -> None:
        for call_id, body, replace in GateMediator._result_slots(self, provider, payload):
            try:
                reservation = self.ledger.reserve_result(context.trajectory_id, provider, call_id, body)
                tracked = self.ledger.call(context.trajectory_id, provider, call_id)
            except LifecycleLedgerError as error:
                raise GateMediationError(str(error)) from error
            if reservation == "replay":
                replace(tracked.delivered_result)
                continue
            binding = self.ledger.spawn_for_call(context.trajectory_id, call_id)
            gate = self._gate(context.trajectory_id)
            if binding:
                receipt_agent_id = async_spawn_receipt(provider, tracked.name, body)
                if not binding.child_trajectory:
                    if receipt_agent_id is not None and context.client == "codex":
                        self._receipt_children[(context.trajectory_id, call_id)] = receipt_agent_id
                        # Receipt data is not authoritative. Release the condition
                        # while the signed child request performs the real binding.
                        deadline = time.monotonic() + 30
                        with self._child_started:
                            while time.monotonic() < deadline:
                                candidate = self.ledger.spawn_for_call(context.trajectory_id, call_id)
                                if candidate and candidate.status == "started" and candidate.child_trajectory == f"codex:thread:{receipt_agent_id}":
                                    binding = candidate
                                    break
                                self._child_started.wait(timeout=min(0.25, deadline - time.monotonic()))
                        if not binding.child_trajectory or binding.status != "started":
                            raise GateMediationError("async spawn receipt did not receive a verified child_start")
                    else:
                        self.trace.write({
                            "direction": "async_receipt_before_binding",
                            "trajectory_id": context.trajectory_id,
                            "call_id": call_id,
                            "provider": provider,
                            "name": tracked.name,
                            "recognized_agent_id": bool(receipt_agent_id),
                            "shape": _value_shape(body),
                        })
                        raise GateMediationError("parent received a child result before an exact child binding")
                if receipt_agent_id is not None:
                    if context.client == "codex" and binding.child_trajectory != f"codex:thread:{receipt_agent_id}":
                        raise GateMediationError("async spawn receipt does not match the signed child carrier")
                    if context.client == "claude" and binding.child_trajectory != f"claude:agent:{receipt_agent_id}":
                        raise GateMediationError("async spawn receipt does not match the signed child carrier")
                    decision = self._decide(
                        context.trajectory_id,
                        "async_spawn_ack",
                        {"call_id": call_id, "child": binding.child_trajectory},
                        lambda: gate.async_spawn_ack(call_id, binding.child_trajectory, body),
                    )
                    if decision.name != "ack":
                        raise GateMediationError("OpenAPPA withheld the async spawn acknowledgement", code="APPA_CHILD_RETURN_BLOCKED")
                    self.ledger.complete_result(context.trajectory_id, provider, call_id, body, body)
                    replace(body)
                    continue
                if binding.status != "started":
                    self.trace.write({
                        "direction": "async_receipt_unrecognized",
                        "trajectory_id": context.trajectory_id,
                        "call_id": call_id,
                        "provider": provider,
                        "name": tracked.name,
                        "shape": _value_shape(body),
                    })
                    raise GateMediationError("parent received a child result before runtime child_start")
                child_context = LifecycleContext(binding.child_trajectory, context.client, binding.principal_scope, "child", context.trajectory_id, call_id)
                gate_value = canonical_child_return(body)
                return_decision = self._decide(context.trajectory_id, "child_return", {"call_id": call_id, "child": binding.child_trajectory}, lambda: gate.child_return(
                    parent_trajectory_id=context.trajectory_id,
                    parent_call_id=call_id,
                    child_trajectory_id=binding.child_trajectory,
                    result=gate_value,
                ))
                if return_decision.name != "ack":
                    raise GateMediationError("OpenAPPA withheld the child return", code="APPA_CHILD_RETURN_BLOCKED")
                # The adapter caches child_return then uses exactly one native
                # spawn_result when after_result settles the parent call.
                decision = self._decide(context.trajectory_id, "spawn_result", {"call_id": call_id, "child": binding.child_trajectory}, lambda: gate.after_result(call_id, body=gate_value))
                if decision.name != "ack":
                    raise GateMediationError("OpenAPPA withheld the parent spawn result", code="APPA_CHILD_RETURN_BLOCKED")
                delivered = body
                self.ledger.complete_result(context.trajectory_id, provider, call_id, body, delivered)
                replace(delivered)
                continue
            waits = self._wait_bindings.get((provider, context.trajectory_id, call_id))
            if waits:
                try:
                    wait_body = json.loads(body) if isinstance(body, str) else body
                except json.JSONDecodeError as error:
                    raise GateMediationError("wait result is not documented JSON") from error
                if not isinstance(wait_body, dict) or set(wait_body) != {"status", "timed_out"} or not isinstance(wait_body.get("timed_out"), bool) or not isinstance(wait_body.get("status"), dict):
                    raise GateMediationError("wait result has an unsupported status shape")
                expected = {binding.child_trajectory.removeprefix("codex:thread:") for binding in waits}
                if not set(wait_body["status"]) <= expected:
                    raise GateMediationError("wait result includes a foreign child target")
                for binding in waits:
                    target = binding.child_trajectory.removeprefix("codex:thread:")
                    if target not in wait_body["status"]:
                        continue
                    status = wait_body["status"][target]
                    if not isinstance(status, dict) or set(status) != {"completed"} or not isinstance(status["completed"], str):
                        raise GateMediationError("wait child status is not a documented completed value")
                    if binding.status == "returned":
                        continue
                    if binding.status != "started":
                        deadline = time.monotonic() + 30
                        with self._child_started:
                            while time.monotonic() < deadline:
                                candidate = self.ledger.binding_for_child(binding.child_trajectory)
                                if candidate and candidate.status in {"started", "returned"}:
                                    binding = candidate
                                    break
                                self._child_started.wait(timeout=min(0.25, deadline - time.monotonic()))
                        if binding.status == "returned":
                            continue
                        if binding.status != "started":
                            raise GateMediationError("wait completion arrived before verified runtime child_start")
                    decision = self._decide(
                        context.trajectory_id,
                        "child_return",
                        {"call_id": binding.parent_call_id, "child": binding.child_trajectory},
                        lambda binding=binding, status=status: gate.child_return(
                            parent_trajectory_id=context.trajectory_id,
                            parent_call_id=binding.parent_call_id,
                            child_trajectory_id=binding.child_trajectory,
                            result=status["completed"],
                        ),
                    )
                    if decision.name != "ack":
                        raise GateMediationError("OpenAPPA withheld the child return", code="APPA_CHILD_RETURN_BLOCKED")
                    try:
                        self.ledger.mark_child_returned(binding.child_trajectory)
                    except LifecycleLedgerError as error:
                        raise GateMediationError(str(error)) from error
                decision = self._decide(context.trajectory_id, "wait_result", {"call_id": call_id}, lambda: gate.after_result(call_id, body=body))
                if decision.name != "ack":
                    raise GateMediationError("OpenAPPA withheld the wait result", code="APPA_CHILD_RETURN_BLOCKED")
                self.ledger.complete_result(context.trajectory_id, provider, call_id, body, body)
                replace(body)
                continue
            decision = self._decide(context.trajectory_id, "tool_result", {"provider": provider, "call_id": call_id, "body": body}, lambda: gate.after_result(call_id, body=body))
            if decision.name == "ack":
                delivered = body
            elif decision.name == "deliver_value" and "value" in decision.payload:
                delivered = decision.payload["value"]
            elif decision.name == "replace_output" and "output" in decision.payload:
                delivered = decision.payload["output"]
            elif decision.name in {"block", "deny_call", "refuse"}:
                raise GateMediationError("OpenAPPA withheld the tool result")
            else:
                raise GateMediationError("OpenAPPA returned an unsupported result decision")
            try:
                self.ledger.complete_result(context.trajectory_id, provider, call_id, body, delivered)
            except LifecycleLedgerError as error:
                raise GateMediationError(str(error)) from error
            replace(delivered)

    def spawn_marker(self, context: LifecycleContext, provider: str, call_id: str) -> str | None:
        binding = self.ledger.spawn_for_call(context.trajectory_id, call_id)
        return binding.marker if binding and binding.status in {"eligible", "bound", "started"} else None

    def current_anchor(self, context: LifecycleContext) -> str:
        return self.ledger.current_anchor(context.trajectory_id)

    def _gate(self, trajectory_id: str) -> Any:
        with self._lock:
            if trajectory_id not in self._gates:
                self._gates[trajectory_id] = self.gate_factory(trajectory_id)
            return self._gates[trajectory_id]

    def _event(self, context: LifecycleContext, event: str, fields: dict[str, Any], call: Callable[[], Any]) -> None:
        value = {"event": event, **fields}
        try:
            reservation = self.ledger.reserve_event(context.trajectory_id, event, value)
        except LifecycleLedgerError as error:
            raise GateMediationError(str(error)) from error
        if reservation == "replay":
            return
        decision = self._decide(context.trajectory_id, event, fields, call)
        if decision.name != "ack":
            raise GateMediationError(f"OpenAPPA rejected lifecycle event {event}")
        try:
            self.ledger.complete_event(context.trajectory_id, event, value, decision.payload)
        except LifecycleLedgerError as error:
            raise GateMediationError(str(error)) from error

    def _runtime(self, gate: Any, method: str, **fields: Any) -> Any:
        callback = getattr(gate, method, None)
        if not callable(callback):
            raise GateMediationError(f"OpenAPPA runtime lacks required lifecycle API {method}")
        return callback(**fields)

    def _decide(self, trajectory_id: str, event: str, fields: dict[str, Any], call: Callable[[], Any]) -> Any:
        self.trace.write({"direction": "request", "trajectory_id": trajectory_id, "event": event, **fields})
        try:
            decision = call()
        except GateMediationError as error:
            self.trace.write({"direction": "failure", "trajectory_id": trajectory_id, "event": event, "error": str(error)})
            raise
        except Exception as error:
            self.trace.write({"direction": "failure", "trajectory_id": trajectory_id, "event": event, "error": str(error)})
            raise GateMediationError("OpenAPPA runtime is unavailable or returned an unusable decision", code="APPA_RUNTIME_UNAVAILABLE") from error
        self.trace.write({"direction": "decision", "trajectory_id": trajectory_id, "event": event, "decision": decision.name, "payload": decision.payload})
        return decision


def parse_route(raw_path: str) -> tuple[str, str, str] | None:
    parsed = urlsplit(raw_path)
    path = parsed.path
    mode = "native"
    if path.startswith("/rewrite/"):
        mode = "rewrite"
        path = path[len("/rewrite"):]
    elif path.startswith("/inject/"):
        mode = "inject"
        path = path[len("/inject"):]
    for provider in PROVIDERS:
        prefix = f"/{provider}/"
        if path.startswith(prefix):
            if mode == "inject" and provider != "anthropic":
                return None
            suffix = path[len(prefix):]
            target = "/" + suffix
            query = [(name, value) for name, value in parse_qsl(parsed.query, keep_blank_values=True) if not _is_sensitive_name(name)]
            if query:
                target += "?" + urlencode(query)
            return provider, mode, target
    return None


def load_provider_keys(path: str | Path) -> dict[str, str]:
    """Load only the three provider credentials from a private regular JSON file."""
    keys_path = Path(path)
    file_stat = keys_path.stat()
    if not stat.S_ISREG(file_stat.st_mode) or file_stat.st_mode & 0o077:
        raise ValueError("--keys-file must be a private regular file (mode 0600)")
    with keys_path.open("r", encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict) or set(value) != KEY_FIELDS:
        raise ValueError("--keys-file must contain exactly the supported provider key fields")
    if not all(isinstance(value[name], str) and value[name] for name in KEY_FIELDS):
        raise ValueError("--keys-file provider key fields must be non-empty strings")
    return {name: value[name] for name in KEY_FIELDS}


def load_private_bytes(path: str | Path) -> bytes:
    key_path = Path(path)
    key_stat = key_path.stat()
    if not stat.S_ISREG(key_stat.st_mode) or key_stat.st_mode & 0o077:
        raise ValueError("--lifecycle-anchor-key-file must be a private regular file (mode 0600)")
    value = key_path.read_bytes()
    if len(value) < 32:
        raise ValueError("--lifecycle-anchor-key-file must contain at least 32 bytes")
    return value


def parse_runtime_url(value: str) -> tuple[str, str]:
    parsed = urlsplit(value)
    if parsed.scheme != "http" or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("--appa-runtime must be an absolute http URL without credentials, query, or fragment")
    try:
        port = parsed.port
    except ValueError as error:
        raise ValueError("--appa-runtime has an invalid port") from error
    netloc = parsed.hostname if port is None else f"{parsed.hostname}:{port}"
    return urlunsplit((parsed.scheme, netloc, parsed.path.rstrip("/"), "", "")), netloc


def parse_mcp_host(value: str) -> str:
    if not isinstance(value, str) or not value or "/" in value or value.count(":") != 1:
        raise ValueError("--appa-mcp-host must be host:port")
    host, port = value.rsplit(":", 1)
    if not host or not port.isdigit() or not 1 <= int(port) <= 65535:
        raise ValueError("--appa-mcp-host must be host:port with a valid port")
    return value


def load_lifecycle_gate_factory(spec: str, runtime_url: str, mcp_host: str) -> Callable[[str], Any]:
    module_name, separator, attribute = spec.partition(":")
    if not module_name or not separator or not attribute:
        raise ValueError("--lifecycle-gate-factory must be module:callable")
    try:
        factory = getattr(importlib.import_module(module_name), attribute, None)
    except ImportError as error:
        raise ValueError(f"--lifecycle-gate-factory module cannot be imported: {module_name}") from error
    if not callable(factory):
        raise ValueError("--lifecycle-gate-factory must resolve to a callable")
    configured = factory(runtime_url, mcp_host)
    if not callable(configured):
        raise ValueError("--lifecycle-gate-factory must return a root factory when passed runtime URL and MCP host")
    return configured


def parse_archestra_base(value: str) -> tuple[str, str, int | None, str]:
    parsed = urlsplit(value)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("--archestra-base must be an absolute http(s) URL")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError("--archestra-base cannot contain credentials, query, or fragment")
    try:
        port = parsed.port
    except ValueError as error:
        raise ValueError("--archestra-base has an invalid port") from error
    return parsed.scheme, parsed.hostname, port, parsed.path.rstrip("/")


def gateway_target(archestra_base: tuple[str, str, int | None, str], provider: str, target: str) -> str:
    """Map each provider-compatible request onto Archestra's provider route."""
    scheme, host, port, base_path = archestra_base
    parsed_target = urlsplit(target)
    netloc = host if port is None else f"{host}:{port}"
    provider_path = parsed_target.path
    if provider in {"openai", "kimi"} and (provider_path == "/v1" or provider_path.startswith("/v1/")):
        provider_path = provider_path[3:] or "/"
    path = f"{base_path}/v1/{provider}{provider_path}"
    return urlunsplit((scheme, netloc, path, parsed_target.query, ""))


def outbound_headers(
    headers: Any,
    provider: str,
    content_length: int,
    provider_keys: dict[str, str],
    probe_run_id: str,
) -> dict[str, str]:
    result: dict[str, str] = {}
    for name, value in headers.items():
        lower = name.lower()
        if lower in {"host", "content-length", "connection", "proxy-connection"} | INTERNAL_LIFECYCLE_HEADERS:
            continue
        if any(marker in lower for marker in SENSITIVE_HEADER_MARKERS):
            continue
        result[name] = value
    key_name, credential_header = PROVIDERS[provider]
    key = provider_keys[key_name]
    result["Content-Length"] = str(content_length)
    result[credential_header] = key if credential_header == "x-api-key" else f"Bearer {key}"
    # This relay-generated label is intentionally not client-native capture metadata.
    result["X-Archestra-Run-Id"] = probe_run_id
    result = {name: value for name, value in result.items() if name.lower() != "accept-encoding"}
    result["Accept-Encoding"] = "identity"
    return result


class ProxyHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server: "ProxyServer"

    def do_GET(self) -> None:
        self._proxy()

    def do_POST(self) -> None:
        self._proxy()

    def do_PUT(self) -> None:
        self._proxy()

    def do_PATCH(self) -> None:
        self._proxy()

    def do_DELETE(self) -> None:
        self._proxy()

    def log_message(self, _format: str, *_args: Any) -> None:
        return  # Captures are structured in JSONL instead of stderr access logs.

    def _proxy(self) -> None:
        route = parse_route(self.path)
        if route is None:
            self.send_error(404, "Only provider, /rewrite, and Anthropic-only /inject routes are accepted")
            return
        provider, mode, target = route
        self.exchange_id = secrets.token_hex(12)
        probe_run_id = "appa-proxy:" + self.exchange_id
        self.gate_context: RootContext | LifecycleContext | None = None
        self.lifecycle_anchor: str | None = None
        if self.server.gate_mediator and not self.server.lifecycle_mediator and _header_value(self.headers, "x-claude-code-agent-id"):
            self.server.gate_mediator.trace.write({
                "direction": "rejection",
                "event": "identity",
                "reason": "root-only enforcement rejected a Claude child before gate initialization",
            })
            self._send_gate_refusal(GateMediationError("root-only APPA enforcement rejects Claude child requests with an agent ID"))
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            self.send_error(400, "Invalid Content-Length")
            return
        body = self.rfile.read(length) if length else b""
        try:
            mediator = self.server.lifecycle_mediator or self.server.gate_mediator
            if mediator:
                if not body:
                    raise GateMediationError("APPA enforcement requires a structured request with native identity")
                payload = json.loads(body)
                if mode in {"rewrite", "inject"}:
                    payload = rewrite_payload(payload, provider, "inbound", self.server.store)
                self.gate_context = (
                    self.server.lifecycle_mediator.open(self.headers, payload, provider)
                    if self.server.lifecycle_mediator
                    else self.server.gate_mediator.root(self.headers, payload)
                )
                # A detached fork carries already-settled source tool results as
                # immutable history. Re-admitting them against the new root would
                # manufacture calls that never occurred on that root.
                if not (self.server.lifecycle_mediator and self.gate_context.kind == "fork"):
                    mediator.accept_results(provider, self.gate_context, payload)
                mediator.end_previous_turn(self.gate_context)
                if self.server.lifecycle_mediator:
                    self.server.lifecycle_mediator.prompt(self.gate_context, payload, provider)
                else:
                    mediator.prompt(self.gate_context, payload)
                if self.server.lifecycle_mediator:
                    self.lifecycle_anchor = self.server.lifecycle_mediator.current_anchor(self.gate_context)
                outbound_body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
            else:
                outbound_body = body
                if mode in {"rewrite", "inject"} and body:
                    outbound_body = json.dumps(
                        rewrite_payload(json.loads(body), provider, "inbound", self.server.store), separators=(",", ":")
                    ).encode("utf-8")
            headers = outbound_headers(
                self.headers, provider, len(outbound_body), self.server.provider_keys, probe_run_id
            )
        except GateMediationError as error:
            self._send_gate_refusal(error)
            return
        except (RuntimeError, UnicodeDecodeError, json.JSONDecodeError) as error:
            self.send_error(400 if not isinstance(error, RuntimeError) else 503, str(error))
            return
        self.actual_upstream = gateway_target(self.server.archestra_base, provider, target)
        upstream = urlsplit(self.actual_upstream)

        self.server.logger.write({
            "kind": "request",
            "exchange_id": self.exchange_id,
            "provider": provider,
            "mode": mode,
            "method": self.command,
            "path": target,
            "actual_upstream": self.actual_upstream,
            "probe_run_id": probe_run_id,
            "headers": safe_headers(self.headers),
            "header_names": sorted(name.lower() for name in self.headers),
            "body": safe_body(body),
            "upstream_body": safe_body(outbound_body),
            "injected_markers": detect_injected_markers(body, self.server.store),
        })
        connection_type = http.client.HTTPSConnection if upstream.scheme == "https" else http.client.HTTPConnection
        connection = connection_type(upstream.hostname, port=upstream.port, timeout=60)
        try:
            request_target = upstream.path + (f"?{upstream.query}" if upstream.query else "")
            connection.request(self.command, request_target, body=outbound_body, headers=headers)
            response = connection.getresponse()
            is_sse = "text/event-stream" in response.getheader("Content-Type", "").lower()
            if is_sse:
                self._relay_sse(response, provider, mode)
            else:
                self._relay_body(response, provider, mode)
        except (OSError, http.client.HTTPException) as error:
            self.send_error(502, f"Archestra relay failed: {error}")
        finally:
            connection.close()

    def _send_response_headers(self, response: http.client.HTTPResponse, content_length: int | None) -> None:
        self.send_response(response.status, response.reason)
        for name, value in response.getheaders():
            if name.lower() in {"connection", "content-length", "transfer-encoding"}:
                continue
            self.send_header(name, value)
        if content_length is not None:
            self.send_header("Content-Length", str(content_length))
        else:
            self.send_header("Connection", "close")
            self.close_connection = True
        if self.lifecycle_anchor:
            self.send_header("X-Appa-Context-Anchor", self.lifecycle_anchor)
        self.end_headers()

    def _send_gate_refusal(self, error: GateMediationError) -> None:
        body = gate_refusal_body(error)
        self.send_response(403, "OpenAPPA enforcement refused the request")
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        self.server.logger.write({
            "kind": "gate_refusal",
            "exchange_id": getattr(self, "exchange_id", None),
            "reason": str(error),
            "remedy_available": error.remedy_available,
        })

    def _relay_body(self, response: http.client.HTTPResponse, provider: str, mode: str) -> None:
        body = response.read()
        mediator = self.server.lifecycle_mediator or self.server.gate_mediator
        if mediator and self.gate_context:
            try:
                try:
                    calls = response_tool_calls(provider, json.loads(body))
                except (UnicodeDecodeError, json.JSONDecodeError):
                    calls = []
                mediator.admit(provider, self.gate_context, calls)
            except (GateMediationError, SpawnArgumentRewriteError) as error:
                self._send_gate_refusal(error)
                return
        if body:
            try:
                transformed = json.loads(body)
                marker_for = None
                if self.server.lifecycle_mediator and self.gate_context:
                    marker_for = lambda call_id: self.server.lifecycle_mediator.spawn_marker(self.gate_context, provider, call_id)
                    if provider != "anthropic":
                        transformed = inject_spawn_carrier(transformed, provider, marker_for)
                logical_response = copy.deepcopy(transformed)
                if mode in {"rewrite", "inject"}:
                    transformed = rewrite_payload(transformed, provider, "outbound", self.server.store)
                if mode == "inject":
                    mapped_marker_for = None
                    if marker_for:
                        mapped_marker_for = lambda opaque: marker_for(self.server.store.original_for(provider, opaque) or opaque)
                    transformed = inject_anthropic_correlation(transformed, self.server.store, mapped_marker_for)
                if self.server.lifecycle_mediator and self.lifecycle_anchor:
                    transformed = stamp_context_carrier(transformed, provider, self.lifecycle_anchor)
                body = json.dumps(transformed, separators=(",", ":")).encode("utf-8")
                if self.server.lifecycle_mediator and self.gate_context:
                    self.server.lifecycle_mediator.register_response(self.gate_context, provider, logical_response, body)
            except SpawnArgumentRewriteError:
                self._send_gate_refusal(GateMediationError("provider spawn arguments could not be safely transformed"))
                return
            except (UnicodeDecodeError, json.JSONDecodeError):
                pass
        self._send_response_headers(response, len(body))
        self.wfile.write(body)
        self.server.logger.write({
            "kind": "response",
            "exchange_id": self.exchange_id,
            "provider": provider,
            "mode": mode,
            "actual_upstream": self.actual_upstream,
            "status": response.status,
            "headers": safe_headers(response.headers),
            "body": safe_body(body),
        })

    def _relay_sse(self, response: http.client.HTTPResponse, provider: str, mode: str) -> None:
        events: list[Any] = []
        marker_for = None
        if self.server.lifecycle_mediator and self.gate_context:
            marker_for = lambda opaque: self.server.lifecycle_mediator.spawn_marker(
                self.gate_context, provider, self.server.store.original_for(provider, opaque) or opaque
            )
        transformer = (
            AnthropicInjectionSSETransformer(self.server.store, events.append, marker_for)
            if mode == "inject"
            else SSETransformer(provider, "outbound" if mode == "rewrite" else None, self.server.store, events.append)
        )
        mediator = self.server.lifecycle_mediator or self.server.gate_mediator
        if mediator and self.gate_context:
            buffer = ToolCallSSEBuffer(provider)
            while chunk := response.read1(4096):
                buffer.feed(chunk)
            try:
                raw, calls = buffer.finish()
                mediator.admit(provider, self.gate_context, calls)
                if provider != "anthropic":
                    raw = rewrite_sse_arguments(raw, provider, marker_for)
            except GateMediationError as error:
                self._send_gate_refusal(error)
                return
            except SpawnArgumentRewriteError:
                self._send_gate_refusal(GateMediationError("provider spawn arguments could not be safely transformed"))
                return
            output = transformer.feed(raw) + transformer.finish()
            if self.server.lifecycle_mediator and self.gate_context:
                self.server.lifecycle_mediator.register_response(self.gate_context, provider, raw, output)
            self._send_response_headers(response, None)
            if output:
                self.wfile.write(output)
                self.wfile.flush()
        else:
            self._send_response_headers(response, None)
            while chunk := response.read1(4096):
                output = transformer.feed(chunk)
                if output:
                    self.wfile.write(output)
                    self.wfile.flush()
            output = transformer.finish()
            if output:
                self.wfile.write(output)
        self.server.logger.write({
            "kind": "response_sse",
            "exchange_id": self.exchange_id,
            "provider": provider,
            "mode": mode,
            "actual_upstream": self.actual_upstream,
            "status": response.status,
            "headers": safe_headers(response.headers),
            "events": redact(events),
        })


class ProxyServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(
        self,
        address: tuple[str, int],
        store: MappingStore,
        logger: CaptureLogger,
        archestra_base: tuple[str, str, int | None, str],
        provider_keys: dict[str, str],
        gate_mediator: GateMediator | None = None,
        lifecycle_mediator: LifecycleGateMediator | None = None,
    ):
        super().__init__(address, ProxyHandler)
        self.store = store
        self.logger = logger
        self.archestra_base = archestra_base
        self.provider_keys = provider_keys
        self.gate_mediator = gate_mediator
        self.lifecycle_mediator = lifecycle_mediator


def main() -> None:
    parser = argparse.ArgumentParser(description="Loopback provider relay with optional tool-ID rewriting")
    parser.add_argument("--port", type=int)
    parser.add_argument("--logs", type=Path, required=True, help="Private directory for redacted relay records")
    parser.add_argument("--db", type=Path, required=True, help="Private SQLite path for provider ID mappings")
    parser.add_argument("--archestra-base", required=True, help="Archestra gateway base URL, for example http://127.0.0.1:9000")
    parser.add_argument("--keys-file", type=Path, default=os.environ.get("APPA_PROVIDER_KEYS_FILE"), help="Private JSON file containing exactly the three provider keys; APPA_PROVIDER_KEYS_FILE may name the file")
    enforcement = parser.add_mutually_exclusive_group()
    enforcement.add_argument("--appa-enforce", action="store_true", help="Fail closed while mediating root-only tool lifecycles through OpenAPPA")
    enforcement.add_argument("--appa-lifecycle-enforce", action="store_true", help="Fail closed while mediating roots, children, forks, and compaction through the lifecycle Gate API")
    parser.add_argument("--appa-runtime", default="http://127.0.0.1:8787", help="Loopback OpenAPPA runtime URL")
    parser.add_argument("--appa-mcp-host", help="Runtime MCP Host header; default derives from --appa-runtime")
    parser.add_argument("--appa-trace", type=Path, help="Private redacted OpenAPPA decision trace")
    parser.add_argument("--lifecycle-ledger", type=Path, help="Private SQLite path for lifecycle state")
    parser.add_argument("--lifecycle-anchor-key-file", type=Path, default=os.environ.get("APPA_LIFECYCLE_ANCHOR_KEY_FILE"), help="Private anchor-key file; APPA_LIFECYCLE_ANCHOR_KEY_FILE may name the file")
    parser.add_argument("--lifecycle-gate-factory", help="Runtime adapter binder module:callable; callable receives runtime URL and MCP host")
    args = parser.parse_args()
    try:
        if args.keys_file is None:
            raise ValueError("--keys-file or APPA_PROVIDER_KEYS_FILE is required")
        if args.appa_enforce and args.appa_trace is None:
            raise ValueError("--appa-trace is required with --appa-enforce")
        archestra_base = parse_archestra_base(args.archestra_base)
        args.appa_runtime, derived_mcp_host = parse_runtime_url(args.appa_runtime)
        args.appa_mcp_host = parse_mcp_host(args.appa_mcp_host or derived_mcp_host)
        provider_keys = load_provider_keys(args.keys_file)
        if args.appa_lifecycle_enforce:
            if not args.appa_trace or not args.lifecycle_ledger or not args.lifecycle_anchor_key_file or not args.lifecycle_gate_factory:
                raise ValueError("lifecycle enforcement requires --appa-trace, --lifecycle-ledger, --lifecycle-anchor-key-file, and --lifecycle-gate-factory")
            anchor_key = load_private_bytes(args.lifecycle_anchor_key_file)
            lifecycle_factory = load_lifecycle_gate_factory(args.lifecycle_gate_factory, args.appa_runtime, args.appa_mcp_host)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        parser.error(str(error))
    store = MappingStore(args.db)
    logger = CaptureLogger(args.logs)
    gate_mediator = None
    lifecycle_mediator = None
    if args.appa_enforce:
        trace = GateTrace(args.appa_trace)
        gate_mediator = GateMediator(
            lambda root_id: Gate(root_id, runtime_url=args.appa_runtime, trace_path=None),
            trace,
        )
    if args.appa_lifecycle_enforce:
        lifecycle_mediator = LifecycleGateMediator(
            lifecycle_factory,
            GateTrace(args.appa_trace),
            LifecycleLedger(args.lifecycle_ledger, anchor_key),
            CheckpointClient(args.appa_runtime),
        )
    port = args.port if args.port is not None else (18770 if args.appa_lifecycle_enforce else 18765)
    server = ProxyServer(("127.0.0.1", port), store, logger, archestra_base, provider_keys, gate_mediator, lifecycle_mediator)
    try:
        server.serve_forever()
    finally:
        server.server_close()
        store.close()
        if lifecycle_mediator:
            lifecycle_mediator.ledger.close()


if __name__ == "__main__":
    main()
