"""Real OpenCode CLI smoke check. Only inference is scripted; no model keys or usage.

Install this directory's locked CLI separately from the lightweight SDK tests.
See ../README.md for invocation and the boundary this check covers.
"""

from __future__ import annotations

import argparse
import base64
import contextlib
import json
import os
import re
import socket
import subprocess
import tempfile
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
PLUGIN = REPO / "marketplace/plugins/opencode"
REMEDY = "appa_execute_remedy_plan"


def content_text(value):
    if isinstance(value, str):
        # V2 encodes failed tool results as JSON inside the provider's content.
        if value.startswith('{"error":'):
            return json.loads(value)["error"]["message"]
        return value
    if isinstance(value, list):
        return "\n".join(item.get("text", "") for item in value)
    return json.dumps(value)


class Fixture:
    """One rigid journey. Every tool result must come from the real host."""

    def __init__(self, output, marker):
        self.output = output
        self.marker = marker
        self.requests = []
        self.receiver = []
        self.errors = []

    def record(self, filename, value):
        with (self.output / filename).open("a") as stream:
            stream.write(json.dumps(value) + "\n")

    def reply(self, body):
        self.record("provider.jsonl", body)
        assert body["model"] == "scripted", "Unexpected model"
        messages = body["messages"]
        prompt = next(
            content_text(m.get("content", ""))
            for m in reversed(messages)
            if m["role"] == "user"
        )
        tools = [t["function"]["name"] for t in body.get("tools", [])]
        if "APPA_SMOKE_READY" in prompt:
            self.advertised = tools
            return None, "Ready."
        assert "APPA_SMOKE_JOURNEY" in prompt, "Unexpected inference request"
        assert REMEDY in tools, "Remedy was not discovered by the actual host"
        assert "execute" not in tools and "subagent" not in tools
        self.requests.append(body)
        assert len(self.requests) <= 5, "Unexpected extra model turn"
        results = [m for m in messages if m["role"] == "tool"]
        step = len(self.requests) - 1
        assert len(results) == step, "Missing or repeated tool result"
        if step == 0:
            return ("read", {"path": "private/smoke.txt"}), None
        if step == 1:
            denial = content_text(results[-1]["content"])
            assert self.marker not in denial, "Private bytes appeared in initial denial"
            assert "public -> internal" in denial, denial
            offers = re.findall(r'offer_id: "([a-f0-9]+)"', denial)
            assert offers, "No real remedy offer"
            self.offer = offers[0]
            return (REMEDY, {"offer_id": self.offer}), None
        if step == 2:
            assert "Authorized" in content_text(results[-1]["content"])
            return ("read", {"path": "private/smoke.txt"}), None
        if step == 3:
            assert self.marker in content_text(results[-1]["content"])
            return ("webfetch", {"url": self.sink}), None
        assert "the readers are not the public audience" in content_text(
            results[-1]["content"]
        )
        return None, "Finished."

    def handler(self):
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                fixture.receiver.append(self.path)
                fixture.record("receiver.jsonl", {"path": self.path})
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"PUBLIC_SINK_CONTACTED")

            def do_POST(self):
                try:
                    assert self.path == "/v1/chat/completions", self.path
                    body = json.loads(
                        self.rfile.read(int(self.headers["Content-Length"]))
                    )
                    call, answer = fixture.reply(body)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.end_headers()
                    delta = {"role": "assistant"}
                    if call:
                        name, arguments = call
                        delta["tool_calls"] = [
                            {
                                "index": 0,
                                "id": f"call_{uuid.uuid4().hex}",
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": json.dumps(arguments),
                                },
                            }
                        ]
                    else:
                        delta["content"] = answer
                    chunk = {
                        "id": "chatcmpl-smoke",
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": "scripted",
                        "choices": [
                            {"index": 0, "delta": delta, "finish_reason": None}
                        ],
                    }
                    self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                    chunk["choices"] = [
                        {
                            "index": 0,
                            "delta": {},
                            "finish_reason": "tool_calls" if call else "stop",
                        }
                    ]
                    self.wfile.write(
                        ("data: " + json.dumps(chunk) + "\n\ndata: [DONE]\n\n").encode()
                    )
                    self.wfile.flush()
                except (
                    AssertionError,
                    ValueError,
                    KeyError,
                    TypeError,
                    StopIteration,
                    OSError,
                ) as error:
                    fixture.errors.append(repr(error))
                    fixture.record("fixture-errors.jsonl", {"error": repr(error)})
                    self.send_error(400, "Scripted fixture rejected the request")

        return Handler


def free_port():
    # APPA needs a concrete port; release an ephemeral reservation before spawn.
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


@contextlib.contextmanager
def process(command, cwd, env, log):
    with log.open("w") as stream:
        child = subprocess.Popen(
            command,
            cwd=cwd,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=stream,
            stderr=subprocess.STDOUT,
        )
        try:
            yield child
        finally:
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def wait_for(check, child, label):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        assert child.poll() is None, f"{label} process exited: {child.returncode}"
        try:
            if check():
                return
        except OSError:
            pass
        time.sleep(0.1)
    raise AssertionError(f"Timed out waiting for {label}")


def get(url, headers=None):
    # Do not inherit a developer's HTTP proxy for loopback test traffic.
    with build_opener(ProxyHandler({})).open(
        Request(url, headers=headers or {}), timeout=2
    ) as response:
        return response.read()


def verify(events, fixture, project):
    assert not fixture.errors, fixture.errors
    assert len(fixture.requests) == 5, "Journey did not finish"
    assert not any(e["type"] == "error" for e in events), events
    calls = [e for e in events if e["type"] == "tool_use"]
    assert [e["part"]["tool"] for e in calls] == ["read", REMEDY, "read", "webfetch"], (
        calls
    )
    assert len({e["sessionID"] for e in events}) == 1, "Journey changed roots"
    assert len({e["part"]["id"] for e in calls}) == 4, "Duplicate tool call"
    provider_calls = [
        (call["id"], call["function"]["name"])
        for message in fixture.requests[-1]["messages"]
        for call in message.get("tool_calls", [])
    ]
    assert [(e["part"]["id"], e["part"]["tool"]) for e in calls] == provider_calls
    states = [e["part"]["state"] for e in calls]
    assert [s["status"] for s in states] == [
        "error",
        "completed",
        "completed",
        "error",
    ], states
    assert "public -> internal" in states[0]["error"]
    assert fixture.marker not in states[0]["error"]
    assert states[1]["input"]["offer_id"] == fixture.offer
    assert "Authorized" in states[1]["output"]
    assert (
        (project / states[0]["input"]["path"]).resolve()
        == (project / states[2]["input"]["path"]).resolve()
        == project / "private/smoke.txt"
    )
    assert fixture.marker in states[2]["output"]
    assert states[3]["input"]["url"] == fixture.sink
    assert "the readers are not the public audience" in states[3]["error"]
    assert fixture.receiver == ["/control-before", "/control-after"], fixture.receiver


def run(output):
    project = output / "project"
    project.mkdir(parents=True)
    (project / "private").mkdir()
    marker = f"appa-private-{uuid.uuid4().hex}"
    (project / "private/smoke.txt").write_text(marker)
    (output / "appa.toml").write_bytes((HERE.parent / "appa.toml").read_bytes())
    home = output / "home"
    home.mkdir()
    # Whitelist environment, rather than inheriting credentials, providers or config.
    env = {
        "PATH": os.environ["PATH"],
        "HOME": str(home),
        "LANG": "C.UTF-8",
        **{
            f"XDG_{kind}_HOME": str(home / kind.lower())
            for kind in ("CONFIG", "DATA", "CACHE", "STATE")
        },
        "OPENCODE_PASSWORD": uuid.uuid4().hex,
        "OPENCODE_DISABLE_MODELS_FETCH": "1",
    }
    cli = str(HERE / "node_modules/.bin/opencode")
    version = subprocess.check_output(
        [cli, "--version"], env=env, text=True, timeout=10
    ).strip()
    expected = json.loads((HERE / "package.json").read_text())["dependencies"][
        "@opencode/cli"
    ]
    assert version == f"opencode v{expected}", version
    assert (
        json.loads((PLUGIN / "package.json").read_text())["dependencies"][
            "@opencode/plugin"
        ]
        == expected
    )
    fixture = Fixture(output, marker)
    server = ThreadingHTTPServer(("127.0.0.1", 0), fixture.handler())
    local = f"http://127.0.0.1:{server.server_port}"
    fixture.sink = f"{local}/public-share?value={marker}"
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        runtime_url = f"http://127.0.0.1:{free_port()}"
        env["APPA_OPENCODE_RUNTIME_URL"] = runtime_url
        (project / "opencode.json").write_text(
            json.dumps(
                {
                    "plugins": [str(PLUGIN)],
                    "model": "smoke/scripted",
                    "providers": {
                        "smoke": {
                            "package": "@opencode/ai/providers/openai-compatible",
                            "settings": {"baseURL": f"{local}/v1"},
                            "models": {
                                "scripted": {
                                    "name": "Local scripted smoke fixture",
                                    "capabilities": {"tools": True},
                                    "limit": {"context": 32000, "output": 2000},
                                }
                            },
                        }
                    },
                    "mcp": {
                        "servers": {
                            "appa": {
                                "type": "remote",
                                "url": f"{runtime_url}/mcp",
                                "oauth": False,
                                "codemode": False,
                            }
                        }
                    },
                    "share": "disabled",
                    "update": "disable",
                }
            )
        )
        binary = str(
            Path(
                os.environ.get("APPA_TEST_BINARY", REPO / "target/debug/appa")
            ).resolve()
        )
        with process(
            [
                binary,
                "runtime",
                "--adapter",
                "opencode",
                "--listen",
                runtime_url.removeprefix("http://"),
                "--config",
                str(output / "appa.toml"),
                "--db",
                str(output / "appa.db"),
            ],
            project,
            env,
            output / "runtime.log",
        ) as runtime:
            wait_for(
                lambda: get(f"{runtime_url}/health") == b"ok", runtime, "APPA health"
            )
            # Port zero lets OpenCode bind atomically; its log reports the selected address.
            host_log = output / "opencode.log"
            with process(
                [cli, "serve", "--hostname", "127.0.0.1", "--port", "0"],
                project,
                env,
                host_log,
            ) as host:
                wait_for(
                    lambda: (
                        "server listening on http://127.0.0.1:" in host_log.read_text()
                    ),
                    host,
                    "OpenCode listen",
                )
                host_url = re.search(
                    r"server listening on (http://127\.0\.0\.1:\d+)",
                    host_log.read_text(),
                )[1]
                headers = {
                    "Authorization": "Basic "
                    + base64.b64encode(
                        f"opencode:{env['OPENCODE_PASSWORD']}".encode()
                    ).decode()
                }
                query = urlencode({"location[directory]": str(project)})

                def connected():
                    status = json.loads(get(f"{host_url}/api/mcp?{query}", headers))
                    (output / "mcp.json").write_text(json.dumps(status))
                    return any(
                        s["name"] == "appa" and s["status"]["status"] == "connected"
                        for s in status["data"]
                    )

                wait_for(connected, host, "APPA MCP discovery")

                def invoke(prompt, name, timeout):
                    with (
                        (output / f"{name}.jsonl").open("w") as stdout,
                        (output / f"{name}.stderr").open("w") as stderr,
                    ):
                        subprocess.run(
                            [
                                cli,
                                "run",
                                "--server",
                                host_url,
                                "--auto",
                                "--format",
                                "json",
                                "--model",
                                "smoke/scripted",
                                "--title",
                                "APPA CLI smoke",
                                prompt,
                            ],
                            cwd=project,
                            env=env,
                            stdin=subprocess.DEVNULL,
                            stdout=stdout,
                            stderr=stderr,
                            timeout=timeout,
                            check=True,
                        )

                # MCP connected is not yet proof that the tool registry has refreshed.
                deadline = time.monotonic() + 30
                probe = 0
                fixture.advertised = []
                while REMEDY not in fixture.advertised:
                    assert time.monotonic() < deadline, (
                        "Remedy never appeared in model tools"
                    )
                    invoke(
                        "APPA_SMOKE_READY",
                        f"ready-{probe}",
                        min(15, deadline - time.monotonic()),
                    )
                    assert not fixture.errors, fixture.errors
                    probe += 1
                assert get(f"{local}/control-before") == b"PUBLIC_SINK_CONTACTED"
                invoke(
                    "APPA_SMOKE_JOURNEY: read private/smoke.txt, execute the offered APPA remedy, retry, then attempt public webfetch.",
                    "journey",
                    60,
                )
                assert get(f"{local}/control-after") == b"PUBLIC_SINK_CONTACTED"
                assert get(f"{runtime_url}/health") == b"ok"
                events = [
                    json.loads(line)
                    for line in (output / "journey.jsonl").read_text().splitlines()
                ]
                verify(events, fixture, project)
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        help="New directory for disposable state and diagnostic evidence",
    )
    args = parser.parse_args()
    output = (
        args.output.resolve()
        if args.output
        else Path(tempfile.mkdtemp(prefix="appa-opencode-smoke-"))
    )
    print(f"Evidence: {output}", flush=True)
    started = time.monotonic()
    run(output)
    print(
        f"PASS: private refusal -> real MCP remedy -> admitted retry -> public refusal; zero receiver requests ({time.monotonic() - started:.1f}s)"
    )
