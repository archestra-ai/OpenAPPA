"""Deterministic Anthropic Messages fixture for the Claude Code gate check.

The fixture drives the real Claude Code harness through two fixed conversations.
It records every model request and chooses tool calls from the actual tool
declarations Claude Code sends. It is not a general Anthropic API emulator.
"""

from __future__ import annotations

import json
import re
import threading
from copy import deepcopy
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import urlsplit

MAX_BODY = 4 * 1024 * 1024
OFFER = re.compile(r'execute_remedy_plan\(offer_id: "(?P<id>[^"]+)"\)')
PATH = re.compile(r"APPA fixture path: (?P<path>[^\n]+)")


def strings(value: Any):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for child in value.values():
            yield from strings(child)
    elif isinstance(value, list):
        for child in value:
            yield from strings(child)


class ModelFixture:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._requests: list[dict[str, Any]] = []
        self._server = _server(self)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self._server.server_port}"

    def start(self) -> ModelFixture:
        self._thread.start()
        return self

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)

    def reset(self) -> None:
        with self._lock:
            self._requests.clear()

    def record(self, request: dict[str, Any]) -> None:
        with self._lock:
            self._requests.append(deepcopy(request))

    def requests(self) -> list[dict[str, Any]]:
        with self._lock:
            return deepcopy(self._requests)


def tool_name(request: dict[str, Any], wanted: str) -> str:
    names = [
        tool.get("name") for tool in request.get("tools", []) if isinstance(tool, dict)
    ]
    exact = [name for name in names if name == wanted]
    matches = exact or [
        name for name in names if isinstance(name, str) and name.endswith(f"__{wanted}")
    ]
    if len(matches) != 1:
        raise ValueError(
            f"tool {wanted!r} is missing or ambiguous in Claude Code declarations: {names}"
        )
    return matches[0]


def conversation(request: dict[str, Any]) -> tuple[str, str]:
    text = "\n".join(strings(request.get("messages", [])))
    match = PATH.search(text)
    if match is None:
        raise ValueError("fixture prompt does not carry an APPA fixture path")
    scenario = "private" if "private.txt" in text else "public"
    return scenario, match.group("path").strip()


def tool_results(request: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        block
        for message in request.get("messages", [])
        if isinstance(message, dict)
        for block in message.get("content", [])
        if isinstance(message.get("content"), list)
        if isinstance(block, dict) and block.get("type") == "tool_result"
    ]


def next_content(request: dict[str, Any]) -> tuple[list[dict[str, Any]], str]:
    scenario, path = conversation(request)
    results = tool_results(request)
    if scenario == "public":
        if not results:
            return [
                tool_use(request, "Write", {"file_path": path, "content": "hello"}, 0)
            ], "tool_use"
        return [{"type": "text", "text": "done"}], "end_turn"

    if not results:
        return [tool_use(request, "Read", {"file_path": path}, 0)], "tool_use"

    latest = "\n".join(strings(results[-1].get("content")))
    if results[-1].get("is_error") and "execute_remedy_plan" in latest:
        offers = OFFER.findall(latest)
        if not offers:
            raise ValueError("APPA denied the read without a parseable remedy offer")
        return [
            tool_use(
                request, "execute_remedy_plan", {"offer_id": offers[0]}, len(results)
            )
        ], "tool_use"
    if "Authorized" in latest:
        return [
            tool_use(
                request,
                "Read",
                {"file_path": path.replace("out.txt", "private.txt")},
                len(results),
            )
        ], "tool_use"
    if "canary-42" in latest:
        return [
            tool_use(
                request,
                "Write",
                {
                    "file_path": path.replace("private.txt", "out.txt"),
                    "content": "canary-42",
                },
                len(results),
            )
        ], "tool_use"
    return [{"type": "text", "text": "the write was blocked"}], "end_turn"


def tool_use(
    request: dict[str, Any], name: str, arguments: dict[str, Any], index: int
) -> dict[str, Any]:
    return {
        "type": "tool_use",
        "id": f"toolu_appa_fixture_{index}",
        "name": tool_name(request, name),
        "input": arguments,
    }


def message(request: dict[str, Any]) -> dict[str, Any]:
    content, stop_reason = next_content(request)
    return {
        "id": "msg_appa_fixture",
        "type": "message",
        "role": "assistant",
        "model": request.get("model", "appa-fixture"),
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": None,
        "usage": {"input_tokens": 1, "output_tokens": 1},
    }


def event_stream(answer: dict[str, Any]) -> bytes:
    start = {**answer, "content": [], "stop_reason": None, "stop_sequence": None}
    events: list[tuple[str, dict[str, Any]]] = [
        ("message_start", {"type": "message_start", "message": start}),
    ]
    for index, block in enumerate(answer["content"]):
        if block["type"] == "text":
            initial = {"type": "text", "text": ""}
            delta = {"type": "text_delta", "text": block["text"]}
        else:
            initial = {
                "type": "tool_use",
                "id": block["id"],
                "name": block["name"],
                "input": {},
            }
            delta = {
                "type": "input_json_delta",
                "partial_json": json.dumps(block["input"]),
            }
        events.extend(
            [
                (
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": index,
                        "content_block": initial,
                    },
                ),
                (
                    "content_block_delta",
                    {"type": "content_block_delta", "index": index, "delta": delta},
                ),
                ("content_block_stop", {"type": "content_block_stop", "index": index}),
            ]
        )
    events.extend(
        [
            (
                "message_delta",
                {
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": answer["stop_reason"],
                        "stop_sequence": None,
                    },
                    "usage": {"output_tokens": 1},
                },
            ),
            ("message_stop", {"type": "message_stop"}),
        ]
    )
    return "".join(
        f"event: {kind}\ndata: {json.dumps(payload)}\n\n" for kind, payload in events
    ).encode()


def _server(fixture: ModelFixture) -> ThreadingHTTPServer:
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def reply(self, status: int, body: bytes, content_type: str) -> None:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self) -> None:
            try:
                length = int(self.headers.get("Content-Length", "-1"))
                if not 0 <= length <= MAX_BODY:
                    raise ValueError("request body exceeds fixture limit")
                request = json.loads(self.rfile.read(length))
                if urlsplit(self.path).path != "/v1/messages" or not isinstance(
                    request, dict
                ):
                    raise ValueError("expected an Anthropic /v1/messages request")
                fixture.record(request)
                answer = message(request)
                if request.get("stream"):
                    self.reply(200, event_stream(answer), "text/event-stream")
                else:
                    self.reply(200, json.dumps(answer).encode(), "application/json")
            except (
                ValueError,
                TypeError,
                AttributeError,
                json.JSONDecodeError,
            ) as error:
                body = json.dumps(
                    {
                        "type": "error",
                        "error": {
                            "type": "invalid_request_error",
                            "message": str(error),
                        },
                    }
                ).encode()
                self.reply(400, body, "application/json")

    return ThreadingHTTPServer(("127.0.0.1", 0), Handler)
