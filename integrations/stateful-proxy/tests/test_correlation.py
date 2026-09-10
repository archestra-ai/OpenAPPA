import json
import unittest

from stateful_proxy.correlation import extract_tool_edges, normalize_identity


class NormalizeIdentityTests(unittest.TestCase):
    def test_claude_structured_user_id_is_session_only_without_agent_inference(self):
        user_id = json.dumps(
            {
                "session_id": "claude-session",
                "device_id": "device-7",
                "account_uuid": "account-8",
            }
        )
        result = normalize_identity({}, {"metadata": {"user_id": user_id}})
        self.assertEqual(result["client"], "claude")
        self.assertEqual(result["identity"]["session_id"], "claude-session")
        self.assertIsNone(result["identity"]["agent_id"])
        self.assertIsNone(result["identity"]["parent_session_id"])
        self.assertEqual(result["trust"], "client_asserted_not_authenticated_authority")

    def test_claude_legacy_and_explicit_agent_metadata(self):
        session_uuid = "550e8400-e29b-41d4-a716-446655440000"
        result = normalize_identity({}, {"user_id": f"user_device_session_{session_uuid}", "agent_id": "agent-7"})
        self.assertEqual(result["identity"]["session_id"], session_uuid)
        self.assertEqual(result["identity"]["agent_id"], "agent-7")
        self.assertEqual(result["provenance"]["session_id"], "claude.legacy_user_id")

    def test_claude_api_key_auth_has_empty_account_uuid(self):
        result = normalize_identity({}, {"metadata": {"user_id": json.dumps({
            "device_id": "fixture-device", "account_uuid": "", "session_id": "fixture-session"
        })}})
        self.assertEqual(result["identity"]["session_id"], "fixture-session")
        legacy = normalize_identity({}, {"metadata": {"user_id": "user_device_account__session_550e8400-e29b-41d4-a716-446655440000"}})
        self.assertEqual(legacy["identity"]["session_id"], "550e8400-e29b-41d4-a716-446655440000")

    def test_claude_rejects_malformed_or_unrecognized_user_ids(self):
        for value in ("claude-session", "user_device_session_not-a-uuid", json.dumps({"session_id": "only-one-field"})):
            result = normalize_identity({}, {"metadata": {"user_id": value}})
            self.assertEqual(result["client"], "claude")
            self.assertIsNone(result["identity"]["session_id"])

    def test_claude_header_only_root_has_session_without_agent_inference(self):
        result = normalize_identity({"X-Claude-Code-Session-Id": "root-session"}, {})
        self.assertEqual(result["client"], "claude")
        self.assertEqual(result["identity"]["session_id"], "root-session")
        self.assertEqual(result["provenance"]["session_id"], "claude.header.session_id")
        self.assertIsNone(result["identity"]["agent_id"])
        self.assertIsNone(result["identity"]["thread_id"])
        self.assertIsNone(result["identity"]["parent_session_id"])
        self.assertEqual(result["trust"], "client_asserted_not_authenticated_authority")

    def test_claude_child_header_and_metadata_share_root_session(self):
        user_id = json.dumps({"session_id": "root-session", "device_id": "device-7", "account_uuid": ""})
        result = normalize_identity(
            {
                "X-Claude-Code-Session-Id": "root-session",
                "X-Claude-Code-Agent-Id": "child-agent",
            },
            {"metadata": {"user_id": user_id}},
        )
        self.assertEqual(result["identity"]["session_id"], "root-session")
        self.assertEqual(result["identity"]["agent_id"], "child-agent")
        self.assertEqual(result["provenance"]["agent_id"], "claude.header.agent_id")
        self.assertIsNone(result["identity"]["thread_id"])
        self.assertFalse(result["conflicts"])

    def test_claude_header_and_body_conflicts_are_visible(self):
        result = normalize_identity(
            {
                "x-claude-code-session-id": "header-session",
                "x-claude-code-agent-id": "header-agent",
            },
            {
                "metadata": {
                    "user_id": json.dumps({"session_id": "body-session", "device_id": "device-7", "account_uuid": ""}),
                    "agent_id": "body-agent",
                }
            },
        )
        self.assertEqual(result["identity"]["session_id"], "header-session")
        self.assertEqual(result["identity"]["agent_id"], "header-agent")
        self.assertEqual({conflict["field"] for conflict in result["conflicts"]}, {"session_id", "agent_id"})

    def test_claude_absent_agent_header_is_not_assumed_to_be_child(self):
        result = normalize_identity(
            {"x-claude-code-session-id": "root-session"},
            {"metadata": {"user_id": json.dumps({"session_id": "root-session", "device_id": "device-7", "account_uuid": ""})}},
        )
        self.assertEqual(result["identity"]["session_id"], "root-session")
        self.assertIsNone(result["identity"]["agent_id"])
        self.assertIsNone(result["identity"]["parent_session_id"])

    def test_codex_canonical_string_is_preferred_and_conflicts_are_visible(self):
        canonical = {
            "session_id": "session-root",
            "thread_id": "thread-child",
            "parent_thread_id": "thread-root",
            "parent_turn_id": "turn-parent",
            "root_turn_id": "turn-root",
            "turn_id": "turn-child",
            "window_id": "thread-child:2",
            "forked_from_thread_id": "fork-origin",
            "forked_from_ordinal_exclusive": 17,
            "request_kind": "compaction",
            "context_window_id": "context-window",
            "window_number": 2,
            "compaction": {"trigger": "auto"},
        }
        result = normalize_identity(
            {
                "X-Codex-Turn-Metadata": json.dumps({**canonical, "session_id": "header-session"}),
                "X-Codex-Parent-Thread-Id": "wrong-parent",
                "X-Codex-Window-Id": "thread-child:99",
            },
            {
                "client_metadata": {
                    "x-codex-turn-metadata": json.dumps(canonical),
                    "session_id": "wrong-session",
                    "thread_id": "wrong-thread",
                },
                "turn_id": "wrong-turn",
            },
        )
        self.assertEqual(result["client"], "codex")
        self.assertEqual(result["identity"]["session_id"], "session-root")
        self.assertEqual(result["identity"]["thread_id"], "thread-child")
        self.assertEqual(result["identity"]["parent_thread_id"], "thread-root")
        self.assertEqual(result["identity"]["forked_from_ordinal_exclusive"], 17)
        self.assertEqual(result["identity"]["compaction"], {"trigger": "auto"})
        self.assertEqual(result["identity"]["request_kind"], "compaction")
        self.assertEqual(result["identity"]["context_window_id"], "context-window")
        self.assertEqual(result["identity"]["window_number"], 2)
        self.assertEqual(result["provenance"]["turn_id"], "codex.canonical_body")
        self.assertGreaterEqual(len(result["conflicts"]), 4)

    def test_codex_flat_body_and_evidenced_header_fallback(self):
        result = normalize_identity(
            {"session-id": "shared", "x-codex-window-id": "child:1"},
            {
                "client_metadata": {
                    "session_id": "shared",
                    "thread_id": "child",
                    "turn_id": "turn",
                    "x-codex-parent-thread-id": "parent",
                }
            },
        )
        self.assertEqual(result["identity"]["session_id"], "shared")
        self.assertEqual(result["identity"]["thread_id"], "child")
        self.assertEqual(result["identity"]["parent_thread_id"], "parent")
        self.assertEqual(result["identity"]["window_id"], "child:1")

    def test_codex_real_header_only_fallback(self):
        result = normalize_identity({"session-id": "shared", "user-agent": "codex_exec/0.153.0"}, {})
        self.assertEqual(result["client"], "codex")
        self.assertEqual(result["identity"]["session_id"], "shared")
        self.assertIsNone(result["identity"]["thread_id"])

    def test_generic_session_header_is_not_assumed_to_be_opencode(self):
        result = normalize_identity({"x-session-id": "generic"}, {})
        self.assertEqual(result["client"], "unknown")
        self.assertIsNone(result["identity"]["session_id"])

    def test_opencode_user_agent_allows_normal_session_header(self):
        result = normalize_identity({"user-agent": "opencode/1.18.29", "x-session-id": "normal"}, {})
        self.assertEqual(result["client"], "opencode")
        self.assertEqual(result["identity"]["session_id"], "normal")

    def test_opencode_normal_and_hosted_headers_report_conflict(self):
        result = normalize_identity(
            {
                "X-Session-Id": "normal",
                "x-session-affinity": "normal",
                "x-opencode-session": "hosted",
                "x-parent-session-id": "parent",
                "x-opencode-request": "message-1",
            },
            {},
        )
        self.assertEqual(result["client"], "opencode")
        self.assertEqual(result["identity"]["session_id"], "hosted")
        self.assertEqual(result["identity"]["parent_session_id"], "parent")
        self.assertEqual(result["identity"]["request_id"], "message-1")
        self.assertTrue(any(item["field"] == "session_id" for item in result["conflicts"]))

    def test_missing_metadata_is_explicitly_empty(self):
        result = normalize_identity(None, "not json")
        self.assertEqual(result["client"], "unknown")
        self.assertFalse(result["conflicts"])
        self.assertTrue(all(value is None for value in result["identity"].values()))


class ToolEdgeTests(unittest.TestCase):
    def test_anthropic_parallel_results_join_by_id_not_position(self):
        edges = extract_tool_edges(
            {
                "messages": [
                    {"role": "assistant", "content": [{"type": "tool_use", "id": "call-a"}, {"type": "tool_use", "id": "call-b"}]},
                    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call-b"}, {"type": "tool_result", "tool_use_id": "call-a"}]},
                ]
            }
        )
        self.assertEqual([edge["call_id"] for edge in edges["edges"]], ["call-b", "call-a"])
        self.assertEqual([(edge["call_index"], edge["result_index"]) for edge in edges["edges"]], [(1, 0), (0, 1)])

    def test_responses_compaction_and_fork_metadata_do_not_create_tool_edges(self):
        body = {
            "client_metadata": {"x-codex-turn-metadata": {"forked_from_thread_id": "fork", "compaction": {"trigger": "auto"}}},
            "output": [
                {"type": "function_call", "call_id": "fn-1", "arguments": "{\\\"call_id\\\": \\\"not-a-protocol-id\\\"}"},
                {"type": "custom_tool_call", "call_id": "custom-1"},
                {"type": "function_call_output", "call_id": "fn-1"},
                {"type": "custom_tool_call_output", "call_id": "custom-1"},
            ],
        }
        edges = extract_tool_edges(body)
        self.assertEqual([call["call_id"] for call in edges["calls"]], ["fn-1", "custom-1"])
        self.assertEqual([edge["call_id"] for edge in edges["edges"]], ["fn-1", "custom-1"])

    def test_chat_completions_and_orphan_result(self):
        edges = extract_tool_edges(
            {
                "choices": [{"message": {"role": "assistant", "tool_calls": [{"id": "chat-a"}, {"id": "chat-b"}]}}],
                "messages": [
                    {"role": "tool", "tool_call_id": "chat-b"},
                    {"role": "tool", "tool_call_id": "missing"},
                ],
            }
        )
        self.assertEqual([edge["call_id"] for edge in edges["edges"]], ["chat-b"])
        self.assertEqual(edges["orphan_results"][0]["call_id"], "missing")

    def test_arbitrary_nested_values_are_not_protocol_edges(self):
        edges = extract_tool_edges(
            {"payload": {"type": "tool_use", "id": "forged"}, "input": [{"type": "message", "content": {"tool_use_id": "forged"}}]}
        )
        self.assertEqual(edges, {"calls": [], "results": [], "edges": [], "orphan_results": []})


if __name__ == "__main__":
    unittest.main()
