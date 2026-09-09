"""Validate the rendered policy and its HTTP command adapters without a cluster."""

import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import threading
import tomllib
import unittest


CHART = Path(__file__).resolve().parents[1]


def render_policy(*arguments):
    rendered = subprocess.run(
        [
            "helm",
            "template",
            "appa-demo",
            str(CHART),
            "--namespace",
            "policy-test",
            "--show-only",
            "templates/configmaps.yaml",
            *arguments,
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    block = rendered.split("  appa.toml: |\n", 1)[1]
    return tomllib.loads("\n".join(line[4:] for line in block.splitlines()))


class PolicyTests(unittest.TestCase):
    def test_timeouts_follow_the_approval_window(self):
        for window in (1, 25, 120, 240, 280):
            with self.subTest(window=window):
                externals = render_policy(
                    "--set",
                    f"mocks.approvalWindowSeconds={window}",
                )["externals"]
                command = externals["authorities"]["change-board"]["command"]
                http_timeout = int(command[command.index("--max-time") + 1])
                self.assertEqual(http_timeout, window + 5)
                self.assertEqual(externals["timeout_ms"], max(30, window + 10) * 1000)
                self.assertEqual(externals["review_timeout_ms"], 600000)
                self.assertEqual(
                    command[-1],
                    "http://appa-demo-mocks.policy-test.svc.cluster.local:8081/approve",
                )

    def test_invalid_approval_windows_are_refused(self):
        for window in ("0", "-1", "1.5", "null", "oops", "281", "300"):
            with self.subTest(window=window):
                with self.assertRaises(subprocess.CalledProcessError):
                    render_policy("--set", f"mocks.approvalWindowSeconds={window}")

    def test_public_sink_stays_checked_and_unused_gates_stay_undeclared(self):
        tools = {tool["name"]: tool for tool in render_policy()["policy"]["tool"]}
        self.assertEqual(
            tools["post_status_update"]["requires"],
            {
                "trust": "trusted",
                "audience": {"contains": ["public"]},
            },
        )
        self.assertEqual(tools["read_secret"]["delta"], {"audience": ["ops"]})
        self.assertNotIn("host/kagent-gate/code_execution", tools)
        self.assertNotIn("host/kagent-gate/memory_persist", tools)
        self.assertNotIn("*", tools)

    def test_http_commands_preserve_input_and_no_answer_bodies(self):
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                self.server.received = (
                    self.headers["Content-Type"],
                    self.rfile.read(int(self.headers["Content-Length"])),
                )
                self.send_response(self.server.status)
                self.send_header("Content-Length", str(len(self.server.body)))
                self.end_headers()
                self.wfile.write(self.server.body)

            def log_message(self, *args):
                pass

        externals = render_policy()["externals"]
        commands = [
            binding["command"]
            for kind in ("annotators", "authorities", "sanitizers")
            for binding in externals[kind].values()
            if binding.get("command", [None])[0] == "/usr/bin/curl"
        ]
        self.assertEqual(len(commands), 5)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            envelope = b'{"version":1,"artifact":{"body":"a\\nb"}}\n'
            for status in (200, 404, 504):
                server.status = status
                server.body = json.dumps(
                    {"version": 1, "answer": {"body": "derived"}}
                    if status == 200
                    else {"error": "no answer"}
                ).encode()
                for command in commands:
                    with self.subTest(status=status, endpoint=command[-1]):
                        result = subprocess.run(
                            [
                                *command[:-1],
                                f"http://127.0.0.1:{server.server_port}/consult",
                            ],
                            input=envelope,
                            capture_output=True,
                            timeout=5,
                            check=True,
                        )
                        self.assertEqual(
                            server.received, ("application/json", envelope)
                        )
                        self.assertEqual(result.stdout, server.body)
        finally:
            server.shutdown()
            thread.join()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
