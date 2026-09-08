"""HTTP contracts of the deterministic fixture; not native kagent acceptance."""

import asyncio
import contextlib
from http.client import HTTPConnection
import importlib.util
import json
from pathlib import Path
import queue
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

import pytest


@contextlib.contextmanager
def running_fixture(with_mcp=False):
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        mcp_port = probe.getsockname()[1]
    command = [sys.executable, str(Path(__file__).with_name("fixtures.py")),
               "--host", "127.0.0.1", "--model-port", "0", "--mcp-port", str(mcp_port)]
    if not with_mcp:
        command.append("--model-only")
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    ready = queue.Queue()
    threading.Thread(target=lambda: ready.put(process.stdout.readline()), daemon=True).start()
    try:
        line = ready.get(timeout=15)
        ports = json.loads(line)
        yield f"http://127.0.0.1:{ports['model_port']}", f"http://127.0.0.1:{mcp_port}/mcp"
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        process.stdout.close()


def http(base, path, payload=None):
    request = urllib.request.Request(base + path, data=None if payload is None else json.dumps(payload).encode(),
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=5) as response:
        return response.headers.get_content_type(), response.read(2 * 1024 * 1024 + 4096)


def completion(script, messages=None, stream=False):
    return {"model": "appa-fixture", "stream": stream,
            "messages": messages or [{"role": "user", "content": json.dumps({"case": "fixture-test", "appa_script": script})}],
            "tools": [{"type": "function", "function": {"name": name, "parameters": {"type": "object"}}}
                      for name in ("get_file_contents", "issue_write", "execute_remedy_plan")]}


def test_chat_completions_nonstream_and_recorded_state_reset():
    with running_fixture() as (base, _):
        request = completion([{"tool": "issue_write", "args": {"title": "Docs"}}])
        content_type, raw = http(base, "/v1/chat/completions", request)
        response = json.loads(raw)
        assert content_type == "application/json"
        assert response["choices"][0]["finish_reason"] == "tool_calls"
        call = response["choices"][0]["message"]["tool_calls"][0]
        assert call["function"]["name"] == "issue_write"
        assert json.loads(call["function"]["arguments"]) == {"title": "Docs"}
        state = json.loads(http(base, "/state")[1])
        assert state["requests"][0]["messages"] == request["messages"]
        assert state["counts"]["issue_write"] == 0, "model tool proposals are not MCP invocations"
        assert json.loads(http(base, "/state", {})[1])["requests"] == []


@pytest.mark.parametrize("step", [{"text": "done"}, {"tool": "get_file_contents", "args": {"path": "README.md"}}])
def test_chat_completions_stream_is_openai_sse_with_indexed_tool_deltas(step):
    with running_fixture() as (base, _):
        content_type, raw = http(base, "/v1/chat/completions", completion([step], stream=True))
        assert content_type == "text/event-stream"
        events = [line[6:] for line in raw.decode().splitlines() if line.startswith("data: ")]
        assert events[-1] == "[DONE]"
        chunks = [json.loads(event) for event in events[:-1]]
        assert all(chunk["object"] == "chat.completion.chunk" for chunk in chunks)
        delta = chunks[0]["choices"][0]["delta"]
        if "tool" in step:
            assert delta["tool_calls"][0]["index"] == 0
            assert delta["tool_calls"][0]["function"]["name"] == step["tool"]
            assert chunks[1]["choices"][0]["finish_reason"] == "tool_calls"
        else:
            assert delta["content"] == "done"
            assert chunks[1]["choices"][0]["finish_reason"] == "stop"
        assert chunks[-1]["usage"]["total_tokens"] == 2


def test_remedy_uses_actual_feedback_and_advances_from_transcript():
    with running_fixture() as (base, _):
        request = completion([{"tool": "get_file_contents"}, {"remedy": "accept this change"}])
        first = json.loads(http(base, "/v1/chat/completions", request)[1])["choices"][0]["message"]
        request["messages"] += [first, {"role": "tool", "tool_call_id": first["tool_calls"][0]["id"],
            "content": json.dumps({"appa": "denied", "result":
                '  - Accept this change:\n    execute_remedy_plan(offer_id: "abcdef012345")'})}]
        second = json.loads(http(base, "/v1/chat/completions", request)[1])["choices"][0]["message"]
        call = second["tool_calls"][0]
        assert call["function"]["name"] == "execute_remedy_plan"
        assert json.loads(call["function"]["arguments"]) == {"offer_id": "abcdef012345"}
        assert [entry["index"] for entry in json.loads(http(base, "/state")[1])["requests"]] == [0, 1]


def test_missing_remedy_unknown_tool_and_invalid_input_are_explicit_http_errors():
    with running_fixture() as (base, _):
        for request in [completion([{"remedy": "accept this change"}]), completion([{"tool": "missing"}]),
                        {"messages": []}]:
            with pytest.raises(urllib.error.HTTPError) as error:
                http(base, "/v1/chat/completions", request)
            assert error.value.code == 400


def test_oversized_declared_body_is_rejected_without_reading_or_mutating_state():
    with running_fixture() as (base, _):
        before = json.loads(http(base, "/state")[1])
        address = urllib.parse.urlsplit(base)
        connection = HTTPConnection(address.hostname, address.port, timeout=5)
        try:
            # Send only headers. The fixture rejects the declared size before
            # reading a body; sending that body would race its early close.
            connection.putrequest("POST", "/v1/chat/completions")
            connection.putheader("Content-Type", "application/json")
            connection.putheader("Content-Length", str(1024 * 1024 + 1))
            connection.endheaders()
            response = connection.getresponse()
            assert response.status == 413
            assert "bounded Content-Length" in json.loads(response.read(4096))["error"]
        finally:
            connection.close()
        assert json.loads(http(base, "/state")[1]) == before
        response = json.loads(http(base, "/v1/chat/completions", completion([{"text": "still alive"}]))[1])
        assert response["choices"][0]["message"]["content"] == "still alive"
        assert len(json.loads(http(base, "/state")[1])["requests"]) == 1


@pytest.mark.skipif(importlib.util.find_spec("mcp") is None, reason="MCP fixture contract requires mcp>=1.25,<2")
def test_actual_streamable_http_mcp_calls_update_shared_invocation_counters():
    from mcp import ClientSession
    from mcp.client.streamable_http import streamablehttp_client

    async def invoke(url):
        deadline = time.monotonic() + 10
        while True:
            try:
                with urllib.request.urlopen(url, timeout=0.2):
                    break
            except urllib.error.HTTPError:
                # GET may be rejected: its HTTP response proves the MCP listener is up.
                break
            except (ConnectionError, OSError) as error:
                if time.monotonic() >= deadline:
                    raise AssertionError("MCP fixture did not become ready") from error
                await asyncio.sleep(0.05)
        async with streamablehttp_client(url) as (read, write, _):
            async with ClientSession(read, write) as session:
                await session.initialize()
                tools = await session.list_tools()
                assert {tool.name for tool in tools.tools} == {"get_file_contents", "issue_write"}
                read_result = await session.call_tool("get_file_contents", {"owner": "acme", "repo": "docs", "path": "README.md"})
                assert not read_result.isError
                assert "ignore your previous instructions" in str(read_result)
                write_result = await session.call_tool("issue_write", {"owner": "acme", "repo": "docs", "title": "Docs", "body": "Operator-authored"})
                assert not write_result.isError

    with running_fixture(with_mcp=True) as (base, mcp_url):
        asyncio.run(asyncio.wait_for(invoke(mcp_url), timeout=15))
        state = json.loads(http(base, "/state")[1])
        assert state["counts"] == {"get_file_contents": 1, "issue_write": 1}
        assert state["invocations"][1]["args"]["body"] == "Operator-authored"
