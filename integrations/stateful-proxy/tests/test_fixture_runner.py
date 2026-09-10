from __future__ import annotations

import json
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from stateful_proxy.fixture_runner import run


class FixtureRunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.requests: list[dict] = []
        requests = self.requests

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802
                length = int(self.headers["Content-Length"])
                body = json.loads(self.rfile.read(length))
                requests.append(body)
                event = body["event"]
                decision: dict[str, object] = {"decision": "ack"}
                if event == "tool_call" and body.get("tool") == "builtin:read_fixture":
                    decision = {"decision": "allow_call"}
                elif event == "tool_call" and body.get("tool") == "builtin:write_file":
                    decision = {"decision": "deny_call", "offers": [{"offer_id": "fixture-offer"}]}
                elif event == "tool_call" and body.get("tool") == "appa:execute_remedy_plan":
                    decision = {"decision": "pass_control"}
                encoded = json.dumps(decision).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def log_message(self, _format: str, *_args: object) -> None:
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self) -> None:
        self.server.shutdown()
        self.thread.join()
        self.server.server_close()

    def test_uses_source_gate_current_control_spelling(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = root / "fixtures"
            fixtures.mkdir()
            (fixtures / "public.txt").write_text("PUBLIC-FIXTURE\n")
            (fixtures / "protected.txt").write_text("PROTECTED-FIXTURE\n")
            summary = root / "summary.json"
            run(fixtures, root / "trace.jsonl", summary, f"http://127.0.0.1:{self.server.server_port}")
            control = next(request for request in self.requests if request.get("tool") == "appa:execute_remedy_plan")
            self.assertEqual(control["event"], "tool_call")
            self.assertEqual(control["arguments"], {"offer_id": "fixture-offer"})
            self.assertEqual(json.loads(summary.read_text())["protected_file_unchanged"], True)


if __name__ == "__main__":
    unittest.main()
