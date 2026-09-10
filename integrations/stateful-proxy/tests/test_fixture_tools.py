from __future__ import annotations

import unittest

from stateful_proxy.fixture_tools import respond, respond_line


class FixtureProtocolTests(unittest.TestCase):
    def test_valid_json_rpc_request_returns_a_result(self) -> None:
        response = respond({"jsonrpc": "2.0", "id": 7, "method": "initialize"})
        self.assertEqual(response["id"], 7)
        self.assertEqual(response["result"]["serverInfo"]["name"], "appa-lifecycle-fixtures")

    def test_non_object_and_wrong_version_are_invalid_requests(self) -> None:
        for request in ([], {"jsonrpc": "1.0", "id": "old", "method": "initialize"}):
            response = respond(request)
            self.assertEqual(response["error"], {"code": -32600, "message": "Invalid Request"})

    def test_malformed_json_is_a_parse_error(self) -> None:
        response = respond_line("{not-json}\n")
        self.assertEqual(response["error"], {"code": -32700, "message": "Parse error"})


if __name__ == "__main__":
    unittest.main()
