import unittest
from stateful_proxy.provider_history import *
class T(unittest.TestCase):
 def test_anthropic_multitext_and_result(self):
  x=canonical_request_history("anthropic",{"messages":[{"role":"user","content":[{"type":"text","text":"a"},{"type":"text","text":"b","cache_control":{}}]},{"role":"assistant","content":[{"type":"tool_use","id":"c","name":"read","input":{"a":1}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":"r"}]}]});self.assertEqual(x[0]["content"][1]["text"],"b");self.assertEqual(x[2]["content"][0]["result"][0]["text"],"r")
 def test_namespace_compaction_bootstrap_source(self):
  x=canonical_request_history("openai",{"previous_response_id":"fake","input":[{"type":"message","id":"fake","role":"developer","content":"d"},{"type":"function_call","call_id":"c","namespace":"mcp__x","name":"n","arguments":"{}"},{"type":"compaction","encrypted_content":"opaque"}]});self.assertEqual(x[0]["namespace"],"mcp__x");self.assertTrue(x[1]["encrypted_content_digest"].startswith("sha256:"));self.assertIsNotNone(x.bootstrap_digest)
 def test_sse_binding_and_exact_prefix(self):
  s=[{"type":"response.output_item.done","item":{"type":"message","id":"server","status":"completed","role":"assistant","content":"issued"}}];x=canonical_response_items("openai",s);self.assertEqual(RootMessageBinding.from_actual_response("openai",s).issued_item_digest,digest_prefix(x));f=canonical_request_history("anthropic",{"messages":[{"role":"assistant","content":"issued"},{"role":"user","content":"fresh"}]});self.assertEqual(find_exact_inherited_prefix(f,x),1)
 def test_private_result_marker_reordered_rejected(self):
  p=canonical_request_history("anthropic",{"messages":[{"role":"assistant","content":"old"}]});a=canonical_request_history("anthropic",{"messages":[{"role":"assistant","content":"old"},{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"private"}]}]});self.assertIsNone(find_exact_inherited_prefix(a,p));m=canonical_request_history("anthropic",{"messages":[{"role":"user","content":"old"}]});self.assertIsNone(find_exact_inherited_prefix(m,p))
 def test_chat_completions_tool_call_and_reordering(self):
  p=canonical_request_history("kimi",{"messages":[{"role":"assistant","content":None,"tool_calls":[{"id":"chat-1","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]},{"role":"tool","tool_call_id":"chat-1","content":"result"}]});self.assertEqual(p[0]["tool_calls"][0]["call_id"],"chat-1");self.assertEqual(p[1]["call_id"],"chat-1");r=canonical_request_history("kimi",{"messages":[{"role":"tool","tool_call_id":"chat-1","content":"result"},{"role":"assistant","content":None,"tool_calls":[{"id":"chat-1","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]}]});self.assertIsNone(find_exact_inherited_prefix(r,p))
 def test_anthropic_sse(self):
  x=canonical_response_items("anthropic",[{"type":"message_start"},{"type":"content_block_start","index":0,"content_block":{"type":"text"}},{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"issued"}},{"type":"content_block_stop","index":0},{"type":"message_stop"}]);self.assertEqual(x[0]["content"],[{"type":"text","text":"issued"}])
 def test_unknown_is_explicit(self):
  with self.assertRaises(UnsupportedHistory):canonical_request_history("anthropic",{"messages":[{"role":"user","content":[{"type":"image"}]}]})
if __name__=="__main__":unittest.main()

class CaptureShapeRegressionTests(unittest.TestCase):
    def test_chat_sse_fragments_preserve_choice_and_tool_order(self):
        events = [
            {"choices": [{"index": 0, "delta": {"content": "hel"}}]},
            {"choices": [{"index": 0, "delta": {"content": "lo", "tool_calls": [{"index": 0, "id": "chat-call", "function": {"name": "read", "arguments": "{\\\"p"}}]}}]},
            {"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\\\":1}"}}]}}]},
        ]
        item = canonical_response_items("kimi", events)[0]
        self.assertEqual(item["content"], [{"type": "text", "text": "hello"}])
        self.assertEqual(item["tool_calls"], [{"type": "function_call", "call_id": "chat-call", "name": "read", "arguments": "{\\\"p\\\":1}"}])

    def test_responses_reasoning_and_custom_tools(self):
        items = canonical_request_history("openai", {"input": [
            {"type": "message", "content": "real Codex default user role"},
            {"type": "reasoning", "encrypted_content": "opaque"},
            {"type": "custom_tool_call", "call_id": "custom-1", "name": "search", "arguments": {"q": "x"}},
            {"type": "custom_tool_call_output", "call_id": "custom-1", "output": {"ok": True}},
        ]})
        self.assertEqual(items[0]["role"], "user")
        self.assertEqual(items[1]["type"], "reasoning")
        self.assertEqual(items[2]["arguments"], {"q": "x"})
        self.assertEqual(items[3]["result"], {"ok": True})

class CapturedResponsesRegressionTests(unittest.TestCase):
    def test_baseline_and_review_relay_shape(self):
        items = canonical_request_history("openai", {"input": [
            {"type": "message", "id": "server-item", "role": "assistant", "phase": "commentary", "content": "visible"},
            {"type": "reasoning", "id": "server-item", "encrypted_content": "opaque", "summary": [{"type": "summary_text", "text": "sanitized"}], "content": []},
            {"type": "tool_search_call", "id": "server-item", "call_id": "search-1", "arguments": "{\"q\":\"x\"}", "execution": {"state": "done"}, "status": "completed"},
            {"type": "tool_search_output", "id": "server-item", "call_id": "search-1", "execution": {"state": "done"}, "tools": [{"type": "function"}], "status": "completed"},
            {"type": "function_call", "id": "server-item", "call_id": "result-id", "name": "read", "namespace": None, "arguments": "{}"},
            {"type": "function_call_output", "id": "server-item", "call_id": "result-id", "output": "result"},
        ]})
        self.assertEqual(items[0]["phase"], "commentary")
        self.assertEqual(items[1]["summary"], [{"type": "summary_text", "text": "sanitized"}])
        self.assertEqual(items[2]["call_id"], items[3]["call_id"])
        self.assertEqual(items[4]["namespace"], None)
        self.assertEqual(items[5]["call_id"], "result-id")

class SyntheticShapeRegressionTests(unittest.TestCase):
    def test_synthetic_openai_input_body_canonicalizes(self):
        body = {"input": [
            {"type": "message", "role": "user", "content": "fixture request"},
            {"type": "function_call", "call_id": "fixture-call", "name": "read", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "fixture-call", "output": "fixture result"},
        ]}
        items = canonical_request_history("openai", body)
        self.assertEqual(items[1]["call_id"], "fixture-call")
        self.assertEqual(items[2]["result"], [{"type": "text", "text": "fixture result"}])

class ReasoningReplayRegressionTests(unittest.TestCase):
    def test_captured_issued_reasoning_equals_replay_with_omitted_empty_content(self):
        emitted = canonical_response_items("openai", {"output": [{
            "type": "reasoning", "id": "server-issued", "encrypted_content": "opaque-ciphertext", "content": [], "summary": []
        }]})
        replay = canonical_request_history("openai", {"input": [{
            "type": "reasoning", "id": "caller-replay-id", "encrypted_content": "opaque-ciphertext", "summary": []
        }]})
        self.assertEqual(emitted, replay)
        self.assertEqual(digest_prefix(emitted), digest_prefix(replay))
        self.assertEqual(find_exact_inherited_prefix(replay, emitted), 1)

    def test_changed_reasoning_payload_never_matches_registered_output(self):
        emitted = canonical_response_items("openai", {"output": [{
            "type": "reasoning", "encrypted_content": "opaque-ciphertext", "content": [], "summary": []
        }]})
        changed_digest = canonical_request_history("openai", {"input": [{
            "type": "reasoning", "encrypted_content": "foreign-ciphertext", "summary": []
        }]})
        changed_summary = canonical_request_history("openai", {"input": [{
            "type": "reasoning", "encrypted_content": "opaque-ciphertext", "summary": [{"type": "summary_text", "text": "changed"}]
        }]})
        self.assertIsNone(find_exact_inherited_prefix(changed_digest, emitted))
        self.assertIsNone(find_exact_inherited_prefix(changed_summary, emitted))

class ReasoningPairRegressionTests(unittest.TestCase):
    def test_synthetic_issued_reasoning_replays_at_next_request(self):
        issued = {"type": "reasoning", "encrypted_content": "fixture-ciphertext", "content": [], "summary": []}
        replay = {"type": "reasoning", "encrypted_content": "fixture-ciphertext", "summary": []}
        emitted = canonical_response_items("openai", {"output": [issued]})
        received = canonical_request_history("openai", {"input": [replay]})
        self.assertEqual(emitted, received)
        self.assertEqual(digest_prefix(emitted), digest_prefix(received))
