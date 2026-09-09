import importlib.util
import io
from unittest.mock import patch
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

SPEC = importlib.util.spec_from_file_location("linear_capture", Path(__file__).with_name("capture.py"))
capture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(capture)


def tool(name):
    return {"name": name, "inputSchema": {"type": "object", "properties": {}}}


class FakeClient:
    def __init__(self, pages):
        self.pages = iter(pages)
        self.calls = []

    def send(self, method, params, notification=False):
        self.calls.append((method, params))
        if method == "initialize":
            return {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}}, "serverInfo": {"name": "fixture"}}
        if method == "notifications/initialized":
            return None
        if method == "tools/list":
            return next(self.pages)
        raise AssertionError("capture must not call tools")


class CaptureTests(unittest.TestCase):
    def test_all_pages_are_captured_without_tool_execution(self):
        client = FakeClient([{"tools": [tool("z")], "nextCursor": "second"}, {"tools": [tool("a")]}])
        result = capture.capture(client)
        self.assertEqual([t["name"] for t in result["tools"]], ["a", "z"])
        self.assertEqual(client.calls[-1], ("tools/list", {"cursor": "second"}))
        self.assertEqual(len(result["sha256"]), 64)
        self.assertNotIn("tools/call", [call[0] for call in client.calls])

    def test_repeated_cursor_and_duplicate_tools_are_not_complete_captures(self):
        for pages in [[{"tools": [tool("a")], "nextCursor": "x"}, {"tools": [], "nextCursor": "x"}],
                      [{"tools": [tool("a")], "nextCursor": "x"}, {"tools": [tool("a")]}],
                      [{"tools": []}], [{"tools": [{"name": "a"}]}]]:
            with self.subTest(pages=pages), self.assertRaises(capture.CaptureError):
                capture.capture(FakeClient(pages))

    def test_sse_selects_matching_reply_after_notifications(self):
        raw = ('data: {"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\r\n\r\n'
               'data: {"jsonrpc":"2.0","id":2,\r\n'
               'data: "result":{"tools":[]}}\r\n\r\n').encode()
        self.assertEqual(capture.decode_response(raw, "text/event-stream", 2), {"tools": []})

    def test_rpc_errors_mismatched_ids_and_duplicate_responses_refuse(self):
        for payload in [{"jsonrpc": "2.0", "id": 2, "error": {"message": "secret"}},
                        {"jsonrpc": "2.0", "id": 3, "result": {}},
                        {"id": 2, "result": {}}]:
            with self.assertRaises(capture.CaptureError):
                capture.decode_response(json.dumps(payload).encode(), "application/json", 2)
        message = 'data: {"jsonrpc":"2.0","id":2,"result":{}}\n\n'
        with self.assertRaises(capture.CaptureError):
            capture.decode_response((message * 2).encode(), "text/event-stream", 2)

    def test_drift_checks_schema_and_description_not_only_tool_names(self):
        before = {"surfaces": {s: {"tools": [tool("a"), tool("removed")]} for s in capture.ENDPOINTS}}
        after = {"surfaces": {s: {"tools": [tool("a") | {"description": "different semantics"}, tool("added")]} for s in capture.ENDPOINTS}}
        for result in capture.drift(capture.fingerprints(before), capture.fingerprints(after)).values():
            self.assertEqual(result, {"added": ["added"], "removed": ["removed"], "changed": ["a"]})

    def test_tool_order_does_not_change_drift(self):
        before = {"surfaces": {s: {"tools": [tool("a"), tool("b")]} for s in capture.ENDPOINTS}}
        after = {"surfaces": {s: {"tools": [tool("b"), tool("a")]} for s in capture.ENDPOINTS}}
        self.assertTrue(all(not items for result in capture.drift(capture.fingerprints(before), capture.fingerprints(after)).values() for items in result.values()))

    def test_lock_contains_hashes_and_detects_nested_schema_changes(self):
        before = {"surfaces": {s: {"tools": [tool("a")]} for s in capture.ENDPOINTS}}
        lock = capture.fingerprints(before)
        after = json.loads(json.dumps(before))
        for surface in after["surfaces"].values():
            surface["tools"][0]["inputSchema"]["properties"]["nested"] = {"type": "object"}
        self.assertNotIn("inputSchema", json.dumps(lock))
        self.assertEqual(len(lock["surfaces"]["read-write"]["tools"]["a"]), 64)
        self.assertEqual(capture.drift(lock, capture.fingerprints(after))["read-write"]["changed"], ["a"])

    def test_cli_defaults_to_lock_and_full_is_explicit(self):
        for full in (False, True):
            clients = [FakeClient([{"tools": [tool("a")]}]) for _ in capture.ENDPOINTS]
            output = io.StringIO()
            with patch.object(capture, "Client", side_effect=clients), patch.dict(os.environ, {capture.TOKEN_ENV: "fixture"}), \
                    patch.object(sys, "argv", ["capture.py"] + (["--full"] if full else [])), patch.object(sys, "stdout", output):
                self.assertEqual(capture.main(), 0)
            result = json.loads(output.getvalue())
            self.assertEqual(result["schema_version"], 1 if full else 2)
            self.assertEqual("inputSchema" in output.getvalue(), full)

    def test_invalid_lock_is_rejected(self):
        for lock in [{}, {"schema_version": 2, "surfaces": {s: {"tools": {"a": "bad"}} for s in capture.ENDPOINTS}}]:
            with self.assertRaises(capture.CaptureError):
                capture.validate_lock(lock)

    def test_cli_help_is_offline_and_missing_credential_emits_no_capture(self):
        env = dict(os.environ)
        env.pop(capture.TOKEN_ENV, None)
        command = [sys.executable, str(Path(__file__).with_name("capture.py"))]
        help_result = subprocess.run(command + ["--help"], capture_output=True, text=True, env=env, timeout=5)
        self.assertEqual(help_result.returncode, 0)
        self.assertIn(capture.TOKEN_ENV, help_result.stdout)
        result = subprocess.run(command, capture_output=True, text=True, env=env, timeout=5)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn(capture.TOKEN_ENV, result.stderr)

    def test_redirect_refuses(self):
        with self.assertRaises(capture.CaptureError):
            capture.NoRedirect().redirect_request(None, None, 302, "", {}, "https://other.example")


if __name__ == "__main__":
    unittest.main()
