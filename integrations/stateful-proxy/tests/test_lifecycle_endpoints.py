from __future__ import annotations

import json
import os
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch

from stateful_proxy.appa_lifecycle_gate import factory


class _Runtime:
    def __init__(self) -> None:
        self.requests: list[tuple[str, str, dict]] = []
        self.agent_calls = 0
        runtime = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])) or b"{}")
                runtime.requests.append((self.path, self.headers["Host"], body))
                if self.path == "/mcp":
                    method = body["method"]
                    if method == "initialize":
                        result = {"protocolVersion": "2025-03-26"}
                    elif method == "tools/list":
                        result = {"tools": [{"name": "execute_remedy_plan"}]}
                    else:
                        result = {"isError": False}
                    response = {"jsonrpc": "2.0", "id": body["id"], "result": result}
                elif body.get("tool") == "agent:example/worker":
                    runtime.agent_calls += 1
                    response = {"decision": "deny_call", "offers": [{"offer_id": "offer"}]} if runtime.agent_calls == 1 else {"decision": "allow_call", "spawn_binding": "binding"}
                elif body.get("tool") == "appa:execute_remedy_plan":
                    response = {"decision": "pass_control"}
                else:
                    response = {"decision": "ack"}
                encoded = json.dumps(response).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                if self.path == "/mcp" and body["method"] == "initialize":
                    self.send_header("Mcp-Session-Id", "fixture-session")
                self.end_headers()
                self.wfile.write(encoded)

            def log_message(self, _format: str, *_args: object) -> None:
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def close(self) -> None:
        self.server.shutdown()
        self.thread.join()
        self.server.server_close()


class LifecycleEndpointTests(unittest.TestCase):
    def test_configured_runtime_handles_root_child_hook_and_mcp(self) -> None:
        runtime = _Runtime()
        self.addCleanup(runtime.close)
        with patch.dict(os.environ, {
            "APPA_LIFECYCLE_SPAWN_TOOL_MAP": '{"Agent":"agent:example/worker"}',
            "APPA_LIFECYCLE_RETURN_FLOOR": '{"trust":"trusted","audience":["public"]}',
        }, clear=False):
            create = factory(runtime.url, "runtime.test:8787")
            parent = create("root")
            child = create("child")
            self.assertEqual(parent.session_start().name, "ack")
            self.assertEqual(parent.before_call("spawn", "Agent", {}).name, "allow_call")
            self.assertEqual(child.child_start(trajectory_id="child", parent_trajectory_id="root", parent_call_id="spawn", principal_scope="scope", inherited_checkpoint="checkpoint").name, "ack")
        self.assertIs(child._family_gate, parent._family_gate)
        self.assertTrue(any(path == "/hook" for path, _, _ in runtime.requests))
        self.assertTrue(any(path == "/mcp" and host == "runtime.test:8787" for path, host, _ in runtime.requests))
        self.assertTrue(any(body.get("child_id") == "child" for path, _, body in runtime.requests if path == "/hook"))

    def test_same_root_on_second_endpoint_is_not_reused(self) -> None:
        first, second = _Runtime(), _Runtime()
        self.addCleanup(first.close)
        self.addCleanup(second.close)
        one = factory(first.url, "first.test:8787")("same-root")
        two = factory(second.url, "second.test:8787")("same-root")
        self.assertIsNot(one, two)
        self.assertEqual(one.session_start().name, "ack")
        self.assertEqual(two.session_start().name, "ack")
        self.assertTrue(any(path == "/hook" for path, _, _ in first.requests))
        self.assertTrue(any(path == "/hook" for path, _, _ in second.requests))


if __name__ == "__main__":
    unittest.main()
