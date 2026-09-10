import json
import hashlib
import sqlite3
import threading
import time
import tempfile
import threading
import unittest
from pathlib import Path

from stateful_proxy.proxy import (
    AnthropicInjectionSSETransformer,
    async_spawn_receipt,
    GateMediationError,
    GateMediator,
    GateTrace,
    LifecycleGateMediator,
    LifecycleContext,
    MappingStore,
    SSETransformer,
    ToolCall,
    ToolCallSSEBuffer,
    correlation_marker,
    detect_injected_markers,
    enforcement_root_context,
    gateway_target,
    gate_refusal_body,
    inject_anthropic_correlation,
    inject_spawn_carrier,
    lifecycle_context,
    load_provider_keys,
    outbound_headers,
    parse_archestra_base,
    parse_route,
    rewrite_payload,
    response_tool_calls,
    safe_headers,
)
from stateful_proxy.appa_gate import GateError
from stateful_proxy.lifecycle_ledger import LifecycleLedger, LifecycleLedgerError
from stateful_proxy.checkpoint_client import Checkpoint
from stateful_proxy.rewritten_arguments import SpawnArgumentRewriteError, is_spawn_tool, rewrite_response_arguments, rewrite_sse_arguments


def sse_data_events(raw: bytes):
    events = []
    for event in raw.decode().strip().split("\n\n"):
        for line in event.splitlines():
            if line.startswith("data: "):
                events.append(json.loads(line[6:]))
    return events


def anthropic_sse(event_type, value):
    return f"event: {event_type}\ndata: {json.dumps(value, separators=(',', ':'))}\n\n".encode()


class FakeDecision:
    def __init__(self, name, payload=None):
        self.name = name
        self.payload = payload or {"decision": name}

    @property
    def allowed(self):
        return self.name == "allow_call"

    @property
    def offers(self):
        offers = self.payload.get("offers")
        return tuple(offers) if isinstance(offers, list) else ()

    @property
    def spawn_binding(self):
        value = self.payload.get("spawn_binding")
        return value if isinstance(value, str) and value else None


class FakeGate:
    def __init__(self, before="allow_call", after="ack", unavailable=False):
        self.before = before
        self.after = after
        self.unavailable = unavailable
        self.before_calls = []
        self.after_calls = []
        self.turn_ends = 0
        self.session_starts = 0

    def session_start(self):
        if self.unavailable:
            raise GateError("runtime unavailable")
        self.session_starts += 1
        return FakeDecision("ack")

    def prompt(self, _text):
        return FakeDecision("ack")

    def before_call(self, call_id, name, args):
        self.before_calls.append((call_id, name, args))
        tool_name = name
        name = self.before.pop(0) if isinstance(self.before, list) else self.before
        payload = {"decision": name, "offers": [{"id": "review"}]}
        if tool_name in {"Agent", "Task", "spawn_agent", "task"} and name == "allow_call":
            payload["spawn_binding"] = "gate-binding-" + call_id
        return FakeDecision(name, payload)

    def after_result(self, call_id, *, body=None, error=None):
        self.after_calls.append((call_id, body, error))
        return FakeDecision(self.after)

    def turn_end(self):
        self.turn_ends += 1
        return FakeDecision("ack")


class LifecycleFakeGate(FakeGate):
    def __init__(self):
        super().__init__()
        self.lifecycle_events = []
        self.private_child_failure = False

    def _event(self, name, **fields):
        self.lifecycle_events.append((name, fields))
        return FakeDecision("ack")

    def child_start(self, *, trajectory_id, parent_trajectory_id, parent_call_id, principal_scope, inherited_checkpoint):
        return self._event("child_start", trajectory_id=trajectory_id, parent_trajectory_id=parent_trajectory_id, parent_call_id=parent_call_id)

    def child_return(self, *, parent_trajectory_id, parent_call_id, child_trajectory_id, result):
        if self.private_child_failure:
            raise GateError("PRIVATE-FIXTURE child answer and arguments")
        return self._event("child_return", parent_trajectory_id=parent_trajectory_id, parent_call_id=parent_call_id, child_trajectory_id=child_trajectory_id, result=result)

    def async_spawn_ack(self, parent_call_id, child_trajectory_id, body):
        return self._event("async_spawn_ack", parent_call_id=parent_call_id, child_trajectory_id=child_trajectory_id, body=body)

    def legacy_child_start(self, **fields):
        return self._event("child_start", **fields)


class ProxyRewriteTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.db = Path(self.temp.name) / "mappings.sqlite3"
        self.store = MappingStore(self.db)

    def tearDown(self):
        self.store.close()
        self.temp.cleanup()

    def test_round_trip_anthropic(self):
        response = {"id": "message_unchanged", "content": [{"type": "tool_use", "id": "provider_tool", "name": "clock"}]}
        rewritten = rewrite_payload(response, "anthropic", "outbound", self.store)
        opaque = rewritten["content"][0]["id"]
        self.assertNotEqual(opaque, "provider_tool")
        self.assertEqual(rewritten["id"], "message_unchanged")
        request = {"messages": [{"role": "user", "content": [{"type": "tool_result", "tool_use_id": opaque}]}]}
        restored = rewrite_payload(request, "anthropic", "inbound", self.store)
        self.assertEqual(restored["messages"][0]["content"][0]["tool_use_id"], "provider_tool")

    def test_interleaved_parallel_mappings(self):
        originals = [f"call-{index}" for index in range(24)]
        replacements: dict[str, str] = {}
        lock = threading.Lock()

        def create(original):
            replacement = self.store.replacement_for("openai", original)
            with lock:
                replacements[original] = replacement

        threads = [threading.Thread(target=create, args=(original,)) for original in originals]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertEqual(len(set(replacements.values())), len(originals))
        for original, replacement in replacements.items():
            self.assertEqual(self.store.original_for("openai", replacement), original)

    def test_restart_persistence_and_unknown_id(self):
        opaque = self.store.replacement_for("openai", "provider-call")
        self.store.close()
        self.store = MappingStore(self.db)
        self.assertEqual(self.store.original_for("openai", opaque), "provider-call")
        unknown = {"type": "function_call_output", "call_id": "unknown", "output": "ok"}
        self.assertEqual(rewrite_payload(unknown, "openai", "inbound", self.store)["call_id"], "unknown")

    def test_sse_split_chunks_and_repeat_delta(self):
        payload = {"type": "content_block_start", "content_block": {"type": "tool_use", "id": "anthropic-original"}}
        event = b"event: content_block_start\n" + b"data: " + json.dumps(payload).encode() + b"\n\n"
        transformer = SSETransformer("anthropic", "outbound", self.store)
        result = b"".join(transformer.feed(event[index:index + 5]) for index in range(0, len(event), 5)) + transformer.finish()
        rewritten = json.loads(next(line[6:] for line in result.decode().splitlines() if line.startswith("data: ")))
        opaque = rewritten["content_block"]["id"]
        repeat = b"data: " + json.dumps({"type": "content_block_delta", "delta": {"type": "input_json_delta"}, "content_block": {"type": "tool_use", "id": "anthropic-original"}}).encode() + b"\n\n"
        transformer = SSETransformer("anthropic", "outbound", self.store)
        repeated = transformer.feed(repeat) + transformer.finish()
        repeated_json = json.loads(next(line[6:] for line in repeated.decode().splitlines() if line.startswith("data: ")))
        self.assertEqual(repeated_json["content_block"]["id"], opaque)

    def test_multiple_openai_style_providers_and_chat_completions(self):
        response = {"output": [{"type": "function_call", "call_id": "same-provider-id"}]}
        openai = rewrite_payload(response, "openai", "outbound", self.store)["output"][0]["call_id"]
        kimi = rewrite_payload(response, "kimi", "outbound", self.store)["output"][0]["call_id"]
        self.assertNotEqual(openai, kimi)
        chat = {"choices": [{"message": {"tool_calls": [{"id": "chat-original", "type": "function"}]}}]}
        rewritten_chat = rewrite_payload(chat, "openai", "outbound", self.store)
        opaque = rewritten_chat["choices"][0]["message"]["tool_calls"][0]["id"]
        replay = {"role": "assistant", "tool_calls": [{"id": opaque, "type": "function"}]}
        self.assertEqual(rewrite_payload(replay, "openai", "inbound", self.store)["tool_calls"][0]["id"], "chat-original")

    def test_fixture_mcp_tool_identity_is_preserved_for_claude_and_codex(self):
        claude = response_tool_calls("anthropic", {"content": [{
            "type": "tool_use", "id": "claude-read", "name": "mcp__appa_fixture__read_source", "input": {"source": "public"},
        }]})
        self.assertEqual(claude, [ToolCall("claude-read", "mcp__appa_fixture__read_source", {"source": "public"})])
        codex = response_tool_calls("openai", {"output": [{
            "type": "function_call", "call_id": "codex-read", "namespace": "mcp__appa_fixture", "name": "read_source", "arguments": '{"source":"public"}',
        }]})
        self.assertEqual(codex, [ToolCall("codex-read", "mcp__appa_fixture.read_source", {"source": "public"})])
        protected = response_tool_calls("openai", {"output": [{
            "type": "function_call", "call_id": "codex-protected", "namespace": "mcp__appa_fixture", "name": "protected_publish", "arguments": '{"text":"x"}',
        }]})
        self.assertEqual(protected[0].name, "mcp__appa_fixture.protected_publish")
        legacy = response_tool_calls("kimi", {"output": [{
            "type": "function_call", "call_id": "kimi-read", "namespace": "", "name": "appa_fixture_read_source", "arguments": '{"source":"public"}',
        }]})
        self.assertEqual(legacy, [ToolCall("kimi-read", "appa_fixture_read_source", {"source": "public"})])

    def test_empty_payloads_no_tool_events_and_routes(self):
        payload = {"id": "message-id", "content": [{"type": "text", "text": "untrusted text remains text"}]}
        self.assertEqual(rewrite_payload(payload, "anthropic", "outbound", self.store), payload)
        self.assertEqual(parse_route("/rewrite/anthropic/v1/messages?beta=x"), ("anthropic", "rewrite", "/v1/messages?beta=x"))
        self.assertEqual(parse_route("/inject/anthropic/v1/messages"), ("anthropic", "inject", "/v1/messages"))
        self.assertEqual(parse_route("/openai/v1/responses"), ("openai", "native", "/v1/responses"))
        self.assertEqual(parse_route("/kimi/v1/chat?api_key=not-forwarded&trace=x"), ("kimi", "native", "/v1/chat?trace=x"))
        self.assertIsNone(parse_route("/inject/openai/v1/responses"))
        self.assertIsNone(parse_route("/not-a-provider/v1"))

    def test_gateway_routes_preserve_provider_protocol_paths(self):
        base = parse_archestra_base("http://127.0.0.1:9000")
        self.assertEqual(
            gateway_target(base, "anthropic", "/v1/messages?beta=tools"),
            "http://127.0.0.1:9000/v1/anthropic/v1/messages?beta=tools",
        )
        self.assertEqual(
            gateway_target(base, "openai", "/v1/responses"),
            "http://127.0.0.1:9000/v1/openai/responses",
        )
        self.assertEqual(
            gateway_target(base, "openai", "/v1/responses/compact"),
            "http://127.0.0.1:9000/v1/openai/responses/compact",
        )
        self.assertEqual(
            gateway_target(base, "kimi", "/v1/chat/completions"),
            "http://127.0.0.1:9000/v1/kimi/chat/completions",
        )
        self.assertEqual(
            gateway_target(base, "openai", "/v1/responses?reason=threshold"),
            "http://127.0.0.1:9000/v1/openai/responses?reason=threshold",
        )

    def test_proxy_generated_run_label_is_forwarded_but_not_client_metadata(self):
        headers = outbound_headers(
            {
                "Authorization": "client-placeholder",
                "X-Archestra-Run-Id": "client-supplied",
                "X-Appa-Spawn-Marker": "spm_not_forwarded",
                "X-Appa-Context-Anchor": "v1.not_forwarded.signature",
            },
            "openai",
            0,
            {"OPENAI_API_KEY": "openai-test", "ANTHROPIC_API_KEY": "anthropic-test", "KIMI_API_KEY": "kimi-test"},
            "appa-proxy:exchange-123",
        )
        self.assertEqual(headers["X-Archestra-Run-Id"], "appa-proxy:exchange-123")
        self.assertNotIn("X-Appa-Spawn-Marker", headers)
        self.assertNotIn("X-Appa-Context-Anchor", headers)
        self.assertEqual(safe_headers({"X-Archestra-Run-Id": "client-supplied"}), {})

    def _mediator(self, gate):
        return GateMediator(lambda _root_id: gate, GateTrace(Path(self.temp.name) / "gate-trace.jsonl"))

    def test_root_absence_vs_claude_child_identity(self):
        root = enforcement_root_context({"X-Claude-Code-Session-Id": "root-session"}, {})
        self.assertEqual(root.root_id, "session:root-session")
        self.assertEqual(
            safe_headers({"X-Claude-Code-Agent-Id": "child-17", "X-Claude-Code-Session-Id": "root-session"}),
            {"x-claude-code-agent-id": "child-17", "x-claude-code-session-id": "root-session"},
        )
        with self.assertRaisesRegex(GateMediationError, "rejects Claude child"):
            enforcement_root_context(
                {"X-Claude-Code-Session-Id": "root-session", "X-Claude-Code-Agent-Id": "child-17"},
                {},
            )

    def test_sse_tool_calls_are_buffered_before_gate_release(self):
        start = anthropic_sse("content_block_start", {
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": "call-1", "name": "Read", "input": {}},
        })
        delta = anthropic_sse("content_block_delta", {
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": '{"path":"alpha.txt"}'},
        })
        stop = anthropic_sse("content_block_stop", {"type": "content_block_stop", "index": 0})
        buffer = ToolCallSSEBuffer("anthropic")
        self.assertIsNone(buffer.feed(start))
        self.assertIsNone(buffer.feed(delta))
        self.assertIsNone(buffer.feed(stop))
        raw, calls = buffer.finish()
        self.assertEqual(raw, start + delta + stop)
        self.assertEqual(calls, [ToolCall("call-1", "Read", {"path": "alpha.txt"})])

    def test_runtime_unavailable_fails_closed_before_model_forwarding(self):
        mediator = self._mediator(FakeGate(unavailable=True))
        with self.assertRaisesRegex(GateMediationError, "runtime is unavailable"):
            mediator.root({"X-Claude-Code-Session-Id": "root-session"}, {})

    def test_missing_identity_fails_closed_without_synthesizing_root(self):
        with self.assertRaisesRegex(GateMediationError, "requires a native client session or thread"):
            enforcement_root_context({}, {"messages": []})

    def test_deny_withholds_original_tool_call_and_reports_admitted_siblings(self):
        gate = FakeGate(before=["allow_call", "deny_call"])
        mediator = self._mediator(gate)
        context = mediator.root({"X-Claude-Code-Session-Id": "root-session"}, {})
        with self.assertRaisesRegex(GateMediationError, "denied the proposed tool call") as raised:
            mediator.admit("anthropic", context, [
                ToolCall("call-1", "Read", {"path": "public.txt"}),
                ToolCall("call-2", "Read", {"path": "secret.txt"}),
            ])
        self.assertNotIn("Read", str(raised.exception))
        self.assertEqual(gate.before_calls, [
            ("call-1", "Read", {"path": "public.txt"}),
            ("call-2", "Read", {"path": "secret.txt"}),
        ])
        self.assertEqual(gate.after_calls, [("call-1", None, "proxy withheld before dispatch")])

    def test_result_replay_conflict_fails_closed(self):
        gate = FakeGate()
        mediator = self._mediator(gate)
        context = mediator.root({"X-Claude-Code-Session-Id": "root-session"}, {})
        mediator.admit("anthropic", context, [ToolCall("call-1", "Read", {"path": "alpha.txt"})])
        original = {"messages": [{"content": [{"type": "tool_result", "tool_use_id": "call-1", "content": "ALPHA"}]}]}
        mediator.accept_results("anthropic", context, original)
        replay = {"messages": [{"content": [{"type": "tool_result", "tool_use_id": "call-1", "content": "ALPHA"}]}]}
        mediator.accept_results("anthropic", context, replay)
        self.assertEqual(len(gate.after_calls), 1)
        conflict = {"messages": [{"content": [{"type": "tool_result", "tool_use_id": "call-1", "content": "BETA"}]}]}
        with self.assertRaisesRegex(GateMediationError, "changed body"):
            mediator.accept_results("anthropic", context, conflict)

    def test_prior_turn_ends_after_results_before_the_next_prompt(self):
        gate = FakeGate()
        mediator = self._mediator(gate)
        context = mediator.root({"X-Claude-Code-Session-Id": "root-session"}, {})
        mediator.prompt(context, {"messages": []})
        mediator.end_previous_turn(context)
        self.assertEqual(gate.turn_ends, 1)

    def test_private_keys_file_requires_exact_schema(self):
        keys_file = Path(self.temp.name) / "provider-keys.json"
        keys_file.write_text(json.dumps({
            "ANTHROPIC_API_KEY": "anthropic-test",
            "OPENAI_API_KEY": "openai-test",
            "KIMI_API_KEY": "kimi-test",
        }))
        keys_file.chmod(0o600)
        self.assertEqual(load_provider_keys(keys_file)["OPENAI_API_KEY"], "openai-test")
        keys_file.write_text(json.dumps({"OPENAI_API_KEY": "openai-test"}))
        with self.assertRaises(ValueError):
            load_provider_keys(keys_file)
        keys_file.chmod(0o644)
        with self.assertRaises(ValueError):
            load_provider_keys(keys_file)

    def test_injects_nonstream_agent_prompt_after_id_rewrite(self):
        payload = {
            "content": [{
                "type": "tool_use",
                "id": "provider-spawn-id",
                "name": "Agent",
                "input": {"prompt": "inspect the repository", "keep": "this value"},
            }]
        }
        rewritten = rewrite_payload(payload, "anthropic", "outbound", self.store)
        injected = inject_anthropic_correlation(rewritten, self.store)
        tool_use = injected["content"][0]
        self.assertEqual(self.store.original_for("anthropic", tool_use["id"]), "provider-spawn-id")
        self.assertEqual(tool_use["input"]["prompt"], f'<appa-correlation parent_call="{tool_use["id"]}"/>inspect the repository')
        self.assertEqual(tool_use["input"]["keep"], "this value")

    def test_chunked_agent_sse_arguments_emit_one_complete_injected_delta(self):
        start = anthropic_sse("content_block_start", {
            "type": "content_block_start",
            "index": 2,
            "content_block": {"type": "tool_use", "id": "provider-spawn-id", "name": "Agent", "input": {}},
        })
        original_input = {"prompt": "delegate this", "nested": {"unchanged": True}}
        encoded = json.dumps(original_input, separators=(",", ":"))
        first, second = encoded[:11], encoded[11:]
        delta_one = anthropic_sse("content_block_delta", {
            "type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": first},
        })
        delta_two = anthropic_sse("content_block_delta", {
            "type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": second},
        })
        stop = anthropic_sse("content_block_stop", {"type": "content_block_stop", "index": 2})
        transformer = AnthropicInjectionSSETransformer(self.store)
        held = start + delta_one + delta_two
        self.assertEqual(b"".join(transformer.feed(held[index:index + 7]) for index in range(0, len(held), 7)), b"")
        released = b"".join(transformer.feed(stop[index:index + 3]) for index in range(0, len(stop), 3)) + transformer.finish()
        events = sse_data_events(released)
        self.assertEqual([event["type"] for event in events], ["content_block_start", "content_block_delta", "content_block_stop"])
        opaque = events[0]["content_block"]["id"]
        self.assertEqual(events[0]["content_block"]["input"], {})
        decoded_input = json.loads(events[1]["delta"]["partial_json"])
        self.assertEqual(decoded_input, {"prompt": f'<appa-correlation parent_call="{opaque}"/>delegate this', "nested": {"unchanged": True}})
        self.assertEqual(self.store.original_for("anthropic", opaque), "provider-spawn-id")

    def test_normal_tool_sse_arguments_are_not_injected(self):
        start = anthropic_sse("content_block_start", {
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "provider-bash-id", "name": "Bash", "input": {}},
        })
        delta = anthropic_sse("content_block_delta", {
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": '{"command":"pwd"}'},
        })
        stop = anthropic_sse("content_block_stop", {"type": "content_block_stop", "index": 0})
        transformer = AnthropicInjectionSSETransformer(self.store)
        output = transformer.feed(start + delta + stop) + transformer.finish()
        events = sse_data_events(output)
        self.assertEqual(json.loads(events[1]["delta"]["partial_json"]), {"command": "pwd"})
        self.assertNotIn("appa-correlation", events[1]["delta"]["partial_json"])
        self.assertEqual(self.store.original_for("anthropic", events[0]["content_block"]["id"]), "provider-bash-id")

    def test_responses_spawn_sse_rewrites_first_delta_and_all_final_representations(self):
        call_id = "resp-spawn"
        original = '{"message":"delegate","fork_context":true}'
        raw = b"".join([
            b"event: response.output_item.added\ndata: " + json.dumps({"type": "response.output_item.added", "sequence_number": 1, "item": {"type": "function_call", "call_id": call_id, "name": "spawn_agent", "namespace": "agents", "arguments": ""}}).encode() + b"\n\n",
            b"event: response.function_call_arguments.delta\ndata: " + json.dumps({"type": "response.function_call_arguments.delta", "sequence_number": 2, "call_id": call_id, "delta": original[:14]}).encode() + b"\n\n",
            b"event: response.function_call_arguments.delta\ndata: " + json.dumps({"type": "response.function_call_arguments.delta", "sequence_number": 3, "call_id": call_id, "delta": original[14:]}).encode() + b"\n\n",
            b"event: response.function_call_arguments.done\ndata: " + json.dumps({"type": "response.function_call_arguments.done", "sequence_number": 4, "call_id": call_id, "arguments": original}).encode() + b"\n\n",
            b"event: response.output_item.done\ndata: " + json.dumps({"type": "response.output_item.done", "sequence_number": 5, "item": {"type": "function_call", "call_id": call_id, "name": "spawn_agent", "namespace": "agents", "arguments": original}}).encode() + b"\n\n",
            b"event: response.completed\ndata: " + json.dumps({"type": "response.completed", "sequence_number": 6, "response": {"usage": {"input_tokens": 3}, "output": [{"type": "function_call", "call_id": call_id, "name": "spawn_agent", "namespace": "agents", "arguments": original}]}}).encode() + b"\n\n",
        ])
        output = rewrite_sse_arguments(raw, "openai", lambda value: "spm_test" if value == call_id else None)
        events = sse_data_events(output)
        self.assertEqual([event["type"] for event in events], ["response.output_item.added", "response.function_call_arguments.delta", "response.function_call_arguments.delta", "response.function_call_arguments.done", "response.output_item.done", "response.completed"])
        self.assertEqual([event["sequence_number"] for event in events], [1, 2, 3, 4, 5, 6])
        self.assertEqual(events[0]["item"]["namespace"], "agents")
        self.assertEqual(events[5]["response"]["usage"], {"input_tokens": 3})
        self.assertEqual(events[2]["delta"], "")
        rewritten = json.loads(events[1]["delta"])
        self.assertEqual(rewritten["fork_context"], True)
        self.assertTrue(rewritten["message"].startswith('<appa-correlation parent_call="resp-spawn" spawn_marker="spm_test"/>'))
        self.assertEqual(json.loads(events[3]["arguments"]), rewritten)
        self.assertEqual(json.loads(events[4]["item"]["arguments"]), rewritten)
        self.assertEqual(json.loads(events[5]["response"]["output"][0]["arguments"]), rewritten)

    def test_chat_spawn_sse_rewrites_fragmented_task_prompt_without_losing_finish_data(self):
        call_id = "chat-task"
        original = '{"prompt":"read alpha","extra":"retained"}'
        raw = b"".join([
            b"data: " + json.dumps({"id": "chat-1", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": call_id, "type": "function", "function": {"name": "task", "arguments": original[:12]}}]}}]}).encode() + b"\n\n",
            b"data: " + json.dumps({"id": "chat-1", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": original[12:]}}]}, "finish_reason": "tool_calls"}], "usage": {"completion_tokens": 7}}).encode() + b"\n\n",
        ])
        output = rewrite_sse_arguments(raw, "kimi", lambda value: "spm_chat" if value == call_id else None)
        events = sse_data_events(output)
        self.assertEqual(events[0]["id"], "chat-1")
        self.assertEqual(events[1]["choices"][0]["finish_reason"], "tool_calls")
        self.assertEqual(events[1]["usage"], {"completion_tokens": 7})
        self.assertEqual(events[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"], "")
        rewritten = json.loads(events[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"])
        self.assertEqual(rewritten["extra"], "retained")
        self.assertTrue(rewritten["prompt"].startswith('<appa-correlation parent_call="chat-task" spawn_marker="spm_chat"/>'))

    def test_unknown_spawn_stream_format_fails_closed(self):
        raw = b"data: " + json.dumps({"type": "response.output_item.added", "item": {"type": "function_call", "call_id": "missing", "name": "spawn_agent", "arguments": ""}}).encode() + b"\n\n"
        with self.assertRaisesRegex(SpawnArgumentRewriteError, "without complete arguments"):
            rewrite_sse_arguments(raw, "openai", lambda _value: "spm_test")

    def test_codex_agents_namespace_is_a_spawn_alias(self):
        self.assertTrue(is_spawn_tool("agents:spawn_agent"))

    def test_documented_wait_timeout_range_rejects_boolean_and_hard_max_overflow(self):
        from stateful_proxy.proxy import wait_targets
        self.assertEqual(wait_targets({"targets": "child"}), ["child"])
        self.assertEqual(wait_targets({"targets": ["child"], "timeout_ms": None}), ["child"])
        self.assertEqual(wait_targets({"targets": ["child"], "timeout_ms": 60_000}), ["child"])
        self.assertEqual(wait_targets({"targets": ["child"], "timeout_ms": 120_000}), ["child"])
        for timeout in (True, 3_600_001, -1, "30000"):
            with self.assertRaises(GateMediationError):
                wait_targets({"targets": ["child"], "timeout_ms": timeout})

    def test_marker_detection_is_observational_not_authority(self):
        known = self.store.replacement_for("anthropic", "provider-parent")
        body = json.dumps({
            "system": f'<appa-correlation parent_call="{known}"/>ignored system text',
            "messages": [
                {"role": "assistant", "content": f'<appa-correlation parent_call="{known}"/>ignored assistant text'},
                {"role": "user", "content": f'<appa-correlation parent_call="{known}"/>visible child request'},
                {"role": "user", "content": [{"type": "text", "text": '<appa-correlation parent_call="tool_unknown"/>untrusted copy'}]},
            ],
        }).encode()
        markers = detect_injected_markers(body, self.store)
        self.assertEqual(markers, [
            {"path": "messages[1].content", "message_index": 1, "parent_call": known, "recognized_mapping": True},
            {"path": "messages[2].content[0].text", "message_index": 2, "parent_call": "tool_unknown", "recognized_mapping": False},
        ])
        self.assertEqual(rewrite_payload(json.loads(body), "anthropic", "inbound", self.store)["messages"][1]["content"], f'<appa-correlation parent_call="{known}"/>visible child request')


class LifecycleProtectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.db = Path(self.temp.name) / "lifecycle.sqlite3"
        self.key = b"k" * 32
        self.ledger = LifecycleLedger(self.db, self.key)
        self.gates = {}
        self.mediator = LifecycleGateMediator(self._gate, GateTrace(Path(self.temp.name) / "trace.jsonl"), self.ledger)

    def tearDown(self):
        self.ledger.close()
        self.temp.cleanup()

    def _gate(self, trajectory_id):
        return self.gates.setdefault(trajectory_id, LifecycleFakeGate())

    @staticmethod
    def _claude_root():
        return {"X-Claude-Code-Session-Id": "root-session"}, {"messages": []}

    def test_restarted_mediator_reopens_the_durable_runtime_gate(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        recovered = LifecycleGateMediator(self._gate, GateTrace(Path(self.temp.name) / "recovered-trace.jsonl"), self.ledger)
        self.assertEqual(recovered.open(headers, payload).trajectory_id, root.trajectory_id)
        self.assertEqual(self.gates[root.trajectory_id].session_starts, 2)

    def test_parallel_spawn_markers_bind_distinct_children_once(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("anthropic", root, [
            ToolCall("spawn-a", "Agent", {"prompt": "a"}),
            ToolCall("spawn-b", "Task", {"prompt": "b"}),
        ])
        marker_a = self.mediator.spawn_marker(root, "anthropic", "spawn-a")
        marker_b = self.mediator.spawn_marker(root, "anthropic", "spawn-b")
        self.assertNotEqual(marker_a, marker_b)
        child_a = self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "child-a"}, {"messages": [{"role": "user", "content": correlation_marker("tool_a", marker_a) + "work"}]})
        child_b = self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "child-b"}, {"messages": [{"role": "user", "content": correlation_marker("tool_b", marker_b) + "work"}]})
        self.assertEqual({child_a.trajectory_id, child_b.trajectory_id}, {"claude:agent:child-a", "claude:agent:child-b"})
        with self.assertRaisesRegex(GateMediationError, "already bound"):
            self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "copied"}, {"messages": [{"role": "user", "content": correlation_marker("tool_a", marker_a) + "work"}]})
        continuation = self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "child-a"}, {"messages": [{"role": "user", "content": "continued child work without bootstrap marker"}]})
        self.assertEqual(continuation.trajectory_id, child_a.trajectory_id)

    def test_captured_claude_child_marker_in_second_text_part_binds(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("anthropic", root, [ToolCall("spawn-captured", "Agent", {"prompt": "delegate"})])
        marker = self.mediator.spawn_marker(root, "anthropic", "spawn-captured")
        child_payload = {"messages": [{"role": "user", "content": [
            {"type": "text", "text": "Reminder: perform only the delegated task."},
            {"type": "text", "text": correlation_marker("tool_captured", marker) + "Read the public fixture."},
        ]}]}
        child = self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "a6114-captured"}, child_payload)
        self.assertEqual(child.trajectory_id, "claude:agent:a6114-captured")

    def test_unknown_child_lifecycle_fails_closed_instead_of_becoming_a_root(self):
        headers, payload = self._claude_root()
        self.mediator.open(headers, payload)
        with self.assertRaisesRegex(GateMediationError, "proxy-issued carrier"):
            self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "unknown-child"}, payload)

    def test_parent_child_return_uses_runtime_events_before_gate_result(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("anthropic", root, [ToolCall("spawn-a", "Agent", {"prompt": "a"})])
        marker = self.mediator.spawn_marker(root, "anthropic", "spawn-a")
        child = self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "child-a"}, {"messages": [{"role": "user", "content": correlation_marker("tool_a", marker) + "work"}]})
        result = {"messages": [{"content": [{"type": "tool_result", "tool_use_id": "spawn-a", "content": "child output"}]}]}
        self.mediator.accept_results("anthropic", root, result)
        self.assertEqual(result["messages"][0]["content"][0]["content"], "child output")
        self.assertEqual([event[0] for event in self.gates[child.trajectory_id].lifecycle_events], ["child_start"])
        self.assertEqual([event[0] for event in self.gates[root.trajectory_id].lifecycle_events], ["child_return"])
        self.assertEqual(self.gates[root.trajectory_id].after_calls, [("spawn-a", "child output", None)])

    def test_captured_anthropic_text_array_is_canonicalized_only_for_gate(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("anthropic", root, [ToolCall("spawn-array", "Agent", {"prompt": "a"})])
        marker = self.mediator.spawn_marker(root, "anthropic", "spawn-array")
        self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "child-array"}, {"messages": [{"role": "user", "content": correlation_marker("tool_array", marker) + "work"}]})
        original = [
            {"type": "text", "text": "The result is PUBLIC-FIXTURE-ALPHA."},
            {"type": "text", "text": "agentId: child-array\n<usage>private SDK metadata</usage>"},
        ]
        result = {"messages": [{"content": [{"type": "tool_result", "tool_use_id": "spawn-array", "content": original}]}]}
        self.mediator.accept_results("anthropic", root, result)
        self.assertEqual(result["messages"][0]["content"][0]["content"], original)
        self.assertEqual(self.gates[root.trajectory_id].lifecycle_events[-1][1]["result"], "The result is PUBLIC-FIXTURE-ALPHA.\nagentId: child-array\n<usage>private SDK metadata</usage>")
        self.assertEqual(self.gates[root.trajectory_id].after_calls[-1], ("spawn-array", "The result is PUBLIC-FIXTURE-ALPHA.\nagentId: child-array\n<usage>private SDK metadata</usage>", None))

    def test_child_return_rejects_nontext_array_blocks(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("anthropic", root, [ToolCall("spawn-bad", "Agent", {"prompt": "a"})])
        marker = self.mediator.spawn_marker(root, "anthropic", "spawn-bad")
        self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "child-bad"}, {"messages": [{"role": "user", "content": correlation_marker("tool_bad", marker) + "work"}]})
        result = {"messages": [{"content": [{"type": "tool_result", "tool_use_id": "spawn-bad", "content": [{"type": "image", "source": "no"}]}]}]}
        with self.assertRaisesRegex(GateMediationError, "non-text"):
            self.mediator.accept_results("anthropic", root, result)

    def test_private_child_return_failure_never_enters_http_refusal(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("anthropic", root, [ToolCall("spawn-a", "Agent", {"prompt": "PRIVATE-FIXTURE args"})])
        marker = self.mediator.spawn_marker(root, "anthropic", "spawn-a")
        self.mediator.open({**headers, "X-Claude-Code-Agent-Id": "child-a"}, {"messages": [{"role": "user", "content": correlation_marker("tool_a", marker) + "work"}]})
        self.gates[root.trajectory_id].private_child_failure = True
        result = {"messages": [{"content": [{"type": "tool_result", "tool_use_id": "spawn-a", "content": "PRIVATE-FIXTURE child return"}]}]}
        with self.assertRaisesRegex(GateMediationError, "runtime is unavailable") as raised:
            self.mediator.accept_results("anthropic", root, result)
        body = gate_refusal_body(raised.exception)
        self.assertNotIn(b"PRIVATE-FIXTURE", body)
        self.assertEqual(json.loads(body), {"error": {"type": "appa_enforcement", "code": "APPA_RUNTIME_UNAVAILABLE", "message": "Policy enforcement is temporarily unavailable."}})
        denied = gate_refusal_body(GateMediationError("PRIVATE-FIXTURE deny feedback", code="APPA_CHILD_TOOL_BLOCKED"))
        self.assertNotIn(b"PRIVATE-FIXTURE", denied)
        self.assertEqual(json.loads(denied)["error"]["message"], "A child tool operation was blocked by policy.")

    def test_inject_route_carries_only_the_admitted_proxy_spawn_marker(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("anthropic", root, [ToolCall("spawn-a", "Agent", {"prompt": "a"})])
        store = MappingStore(Path(self.temp.name) / "mappings.sqlite3")
        try:
            outbound = rewrite_payload({"content": [{"type": "tool_use", "id": "spawn-a", "name": "Agent", "input": {"prompt": "delegate"}}]}, "anthropic", "outbound", store)
            injected = inject_anthropic_correlation(
                outbound,
                store,
                lambda opaque: self.mediator.spawn_marker(root, "anthropic", store.original_for("anthropic", opaque) or opaque),
            )
            opaque = injected["content"][0]["id"]
            marker = self.mediator.spawn_marker(root, "anthropic", "spawn-a")
            self.assertEqual(injected["content"][0]["input"]["prompt"], correlation_marker(opaque, marker) + "delegate")
        finally:
            store.close()

    def test_fork_requires_exact_checkpoint_and_has_independent_anchor(self):
        root_payload = {"client_metadata": {"x-codex-turn-metadata": {"thread_id": "root-thread", "session_id": "operator", "checkpoint_id": "checkpoint-1"}}}
        root = self.mediator.open({"X-Codex-Turn-Metadata": json.dumps(root_payload["client_metadata"]["x-codex-turn-metadata"])}, root_payload)
        self.mediator.prompt(root, root_payload)
        self.ledger.record_checkpoint(root.trajectory_id, "checkpoint-1")
        fork_payload = {"client_metadata": {"x-codex-turn-metadata": {
            "thread_id": "fork-thread", "session_id": "operator", "forked_from_thread_id": "root-thread", "fork_checkpoint_id": "checkpoint-1",
        }}}
        with self.assertRaisesRegex(GateMediationError, "checkpointed provider-response binding"):
            self.mediator.open({"X-Codex-Turn-Metadata": json.dumps(fork_payload["client_metadata"]["x-codex-turn-metadata"])}, fork_payload)
        with self.assertRaisesRegex(GateMediationError, "checkpointed provider-response binding"):
            self.mediator.open({"X-Codex-Turn-Metadata": json.dumps({"thread_id": "other", "session_id": "operator", "forked_from_thread_id": "root-thread", "fork_checkpoint_id": "unknown"})}, {"client_metadata": {"x-codex-turn-metadata": {"thread_id": "other", "session_id": "operator", "forked_from_thread_id": "root-thread", "fork_checkpoint_id": "unknown"}}})

    def test_codex_and_opencode_carriers_use_real_spawn_argument_shapes(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("openai", root, [ToolCall("codex-spawn", "spawn_agent", {"message": "read alpha", "fork_context": True})])
        marker = self.mediator.spawn_marker(root, "openai", "codex-spawn")
        codex = inject_spawn_carrier({"output": [{"type": "function_call", "call_id": "codex-spawn", "name": "spawn_agent", "arguments": '{"message":"read alpha","fork_context":true}'}]}, "openai", lambda call_id: self.mediator.spawn_marker(root, "openai", call_id))
        self.assertTrue(json.loads(codex["output"][0]["arguments"])["message"].startswith(correlation_marker("codex-spawn", marker)))
        self.mediator.admit("openai", root, [ToolCall("open-spawn", "task", {"prompt": "read beta"})])
        opencode = inject_spawn_carrier({"output": [{"type": "function_call", "call_id": "open-spawn", "name": "task", "arguments": '{"prompt":"read beta"}'}]}, "openai", lambda call_id: self.mediator.spawn_marker(root, "openai", call_id))
        self.assertTrue(json.loads(opencode["output"][0]["arguments"])["prompt"].startswith("<appa-correlation"))

    def test_spawn_carrier_is_preallocated_but_gate_authorizes_exact_client_arguments(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        original = {"message": "read alpha", "fork_context": True}
        self.mediator.admit("openai", root, [ToolCall("async-spawn", "spawn_agent", original)])
        gate_args = self.gates[root.trajectory_id].before_calls[-1][2]
        self.assertNotEqual(gate_args, original)
        self.assertTrue(gate_args["message"].startswith('<appa-correlation parent_call="async-spawn" spawn_marker="spm_'))
        binding = self.ledger.spawn_for_call(root.trajectory_id, "async-spawn")
        self.assertIsNotNone(binding)
        self.assertEqual(binding.status, "eligible")
        # An identical provider replay derives the recorded alias. A changed
        # provider argument cannot request an arbitrary new carrier rewrite.
        self.mediator.admit("openai", root, [ToolCall("async-spawn", "spawn_agent", original)])
        self.assertEqual(len(self.gates[root.trajectory_id].before_calls), 1)
        with self.assertRaisesRegex(GateMediationError, "exact recorded spawn argument alias"):
            self.mediator.admit("openai", root, [ToolCall("async-spawn", "spawn_agent", {"message": "changed", "fork_context": True})])

    def test_async_spawn_receipt_is_acknowledged_without_calling_child_return(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("anthropic", root, [ToolCall("tool_async_spawn", "Agent", {"prompt": "read"})])
        marker = self.mediator.spawn_marker(root, "anthropic", "tool_async_spawn")
        child = self.mediator.open(
            {**headers, "X-Claude-Code-Agent-Id": "agent-1"},
            {"messages": [{"role": "user", "content": correlation_marker("tool_async_spawn", marker) + "read"}]},
        )
        receipt = [{"type": "text", "text": "Async agent launched successfully.\nagentId: agent-1 (internal ID - do not mention to user.)"}]
        result = {"messages": [{"content": [{"type": "tool_result", "tool_use_id": "tool_async_spawn", "content": receipt}]}]}
        self.mediator.accept_results("anthropic", root, result)
        self.assertEqual(result["messages"][0]["content"][0]["content"], receipt)
        parent_events = [event[0] for event in self.gates[root.trajectory_id].lifecycle_events]
        self.assertEqual(parent_events, ["async_spawn_ack"])
        self.assertEqual(self.gates[root.trajectory_id].after_calls, [])
        self.assertEqual(self.ledger.spawn_for_call(root.trajectory_id, "tool_async_spawn").status, "started")
        self.assertEqual(child.trajectory_id, "claude:agent:agent-1")

    def test_codex_async_spawn_receipt_is_acknowledged_without_a_birth_child_end(self):
        root_id, scope = "codex:thread:root", "codex:scope:operator"
        self.ledger.ensure_root(root_id, "codex", scope)
        root = LifecycleContext(root_id, "codex", scope, "root")
        self.mediator.admit("openai", root, [ToolCall("codex-spawn", "spawn_agent", {"message": "read"})])
        marker = self.mediator.spawn_marker(root, "openai", "codex-spawn")
        self.ledger.bind_child(marker, "codex:thread:agent-1", "codex", scope)
        self.ledger.mark_child_started("codex:thread:agent-1")
        result = {"input": [{"type": "function_call_output", "call_id": "codex-spawn", "output": {"agent_id": "agent-1", "nickname": "display-only"}}]}
        self.mediator.accept_results("openai", root, result)
        self.assertEqual(result["input"][0]["output"], {"agent_id": "agent-1", "nickname": "display-only"})
        self.assertEqual([event[0] for event in self.gates[root_id].lifecycle_events], ["async_spawn_ack"])
        self.assertEqual(self.gates[root_id].after_calls, [])

    def test_codex_receipt_first_waits_for_started_signed_child_before_ack(self):
        root_id, scope = "codex:thread:receipt-root", "codex:scope:operator"
        self.ledger.ensure_root(root_id, "codex", scope)
        root = LifecycleContext(root_id, "codex", scope, "root")
        self.mediator.admit("openai", root, [ToolCall("receipt-spawn", "spawn_agent", {"message": "read"})])
        marker = self.mediator.spawn_marker(root, "openai", "receipt-spawn")

        def start_child():
            time.sleep(0.01)
            self.ledger.bind_child(marker, "codex:thread:agent-2", "codex", scope)
            self.ledger.mark_child_started("codex:thread:agent-2")
            with self.mediator._child_started:
                self.mediator._child_started.notify_all()

        worker = threading.Thread(target=start_child)
        worker.start()
        result = {"input": [{"type": "function_call_output", "call_id": "receipt-spawn", "output": {"agent_id": "agent-2", "nickname": "display"}}]}
        self.mediator.accept_results("openai", root, result)
        worker.join()
        self.assertEqual(self.ledger.binding_for_child("codex:thread:agent-2").status, "started")
        self.assertEqual([event[0] for event in self.gates[root_id].lifecycle_events], ["async_spawn_ack"])

    def test_codex_async_receipt_rejects_undocumented_fields_and_long_nickname(self):
        self.assertEqual(async_spawn_receipt("openai", "multi_agent_v1:spawn_agent", '{"agent_id":"child","nickname":"display"}'), "child")
        self.assertIsNone(async_spawn_receipt("openai", "spawn_agent", {"agent_id": "child", "extra": "no"}))
        self.assertIsNone(async_spawn_receipt("openai", "spawn_agent", {"agent_id": "child", "nickname": "x" * 129}))

    def test_copied_anchor_in_user_text_or_tool_output_cannot_compact(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        original_anchor = self.ledger.current_anchor(root.trajectory_id)
        compact_payload = {"metadata": {"user_id": json.dumps({"device_id": "d", "account_uuid": "a", "session_id": "root-session"})}, "compaction": True, "messages": [{"role": "user", "content": f'<appa-context anchor="{original_anchor}"/>'}]}
        compact_headers = {"X-Claude-Code-Session-Id": "root-session"}
        compacted = self.mediator.open(compact_headers, compact_payload)
        self.assertTrue(compacted.compaction)
        self.assertEqual(self.ledger.current_anchor(root.trajectory_id), original_anchor)
        copied = self.mediator.open(compact_headers, {**compact_payload, "messages": [{"role": "user", "content": "fresh user prompt"}], "tool_result": f'<appa-context anchor="{original_anchor}"/>'})
        self.assertTrue(copied.compaction)

    def test_structured_codex_compaction_fails_closed_without_registered_response_item(self):
        metadata = {"thread_id": "codex-thread", "session_id": "operator"}
        root_payload = {"client_metadata": {"x-codex-turn-metadata": metadata}}
        root = self.mediator.open({"X-Codex-Turn-Metadata": json.dumps(metadata)}, root_payload)
        anchor = self.ledger.current_anchor(root.trajectory_id)
        compact_metadata = {**metadata, "compaction": {"kind": "compaction"}}
        compact_payload = {"client_metadata": {"x-codex-turn-metadata": compact_metadata}, "messages": [{"role": "user", "content": f'<appa-context anchor="{anchor}"/>'}]}
        compacted = self.mediator.open({"X-Codex-Turn-Metadata": json.dumps(compact_metadata)}, compact_payload)
        self.assertTrue(compacted.compaction)
        self.assertEqual(self.ledger.current_anchor(root.trajectory_id), anchor)

    def test_child_compaction_keeps_the_child_topology_selector(self):
        context = LifecycleContext(
            trajectory_id="claude:agent:child",
            client="claude",
            principal_scope="claude:scope:root",
            kind="child",
            parent_trajectory="claude:session:root",
            parent_call_id="spawn-a",
            compaction=True,
        )
        self.mediator.prompt(context, {"messages": [{"role": "user", "content": "compacted child context"}]})
        self.assertIn(context.trajectory_id, self.gates)
        self.assertNotIn("claude:session:root", self.gates)

    def test_detached_fork_root_and_sibling_taint_are_isolated(self):
        root = "codex:thread:parent"
        scope = "codex:scope:operator"
        self.ledger.ensure_root(root, "codex", scope)
        self.ledger.record_checkpoint(root, "checkpoint-1")
        fork_a = "codex:thread:fork-a"
        fork_b = "codex:thread:fork-b"
        self.ledger.fork(fork_a, root, "checkpoint-1", "codex", scope)
        self.ledger.fork(fork_b, root, "checkpoint-1", "codex", scope)
        self.assertEqual(self.ledger.root_for(fork_a), fork_a)
        marker = self.ledger.issue_spawn_marker(fork_a, "spawn-child", scope, "runtime-binding")
        child = "codex:thread:fork-child"
        self.ledger.bind_child(marker, child, "codex", scope)
        self.assertEqual(self.ledger.root_for(child), fork_a)
        root_anchor = self.ledger.current_anchor(root)
        sibling_anchor = self.ledger.current_anchor(fork_b)
        fork_anchor = self.ledger.current_anchor(fork_a)
        advanced = self.ledger.compact(fork_a, fork_anchor)
        self.assertNotEqual(advanced, fork_anchor)
        self.assertEqual(self.ledger.current_anchor(root), root_anchor)
        self.assertEqual(self.ledger.current_anchor(fork_b), sibling_anchor)
        self.assertEqual(self.ledger.fork(fork_a, root, "checkpoint-1", "codex", scope), advanced)

    def test_restart_preserves_pending_marker_and_rejects_foreign_binding(self):
        headers, payload = self._claude_root()
        root = self.mediator.open(headers, payload)
        self.mediator.admit("anthropic", root, [ToolCall("spawn-a", "Agent", {"prompt": "a"})])
        marker = self.mediator.spawn_marker(root, "anthropic", "spawn-a")
        self.ledger.close()
        self.ledger = LifecycleLedger(self.db, self.key)
        self.mediator = LifecycleGateMediator(self._gate, GateTrace(Path(self.temp.name) / "restart-trace.jsonl"), self.ledger)
        child = lifecycle_context({**headers, "X-Claude-Code-Agent-Id": "child-a"}, {"messages": [{"role": "user", "content": correlation_marker("tool_a", marker) + "work"}]}, self.ledger)
        self.assertEqual(child.parent_trajectory, root.trajectory_id)
        with self.assertRaises(LifecycleLedgerError):
            self.ledger.bind_child(marker, "claude:agent:foreign", "claude", "claude:scope:other")

    def test_interrupted_admission_is_distinct_from_an_idempotent_replay(self):
        self.ledger.ensure_root("claude:session:root", "claude", "claude:scope:root")
        self.assertEqual(self.ledger.reserve_call("claude:session:root", "anthropic", "call-1", "Read", {"path": "a"}), "new")
        self.ledger.close()
        self.ledger = LifecycleLedger(self.db, self.key)
        with self.assertRaisesRegex(LifecycleLedgerError, "unresolved"):
            self.ledger.reserve_call("claude:session:root", "anthropic", "call-1", "Read", {"path": "a"})

    def test_checkpointed_provider_history_creates_one_detached_codex_root(self):
        class Checkpoints:
            def __init__(self): self.forks = []
            def fork(self, checkpoint_id, root_id): self.forks.append((checkpoint_id, root_id))
            def create(self, _root_id): raise AssertionError("not used while opening a fork")
        checkpoints = Checkpoints()
        mediator = LifecycleGateMediator(self._gate, GateTrace(Path(self.temp.name) / "fork-trace.jsonl"), self.ledger, checkpoints)
        source, scope = "codex:thread:source", "codex:scope:operator"
        self.ledger.ensure_root(source, "codex", scope)
        self.ledger.record_checkpoint(source, "checkpoint-source")
        request_prefix = [{"type": "message", "role": "user", "content": [{"type": "text", "text": "source"}]}]
        issued = [{"type": "message", "role": "assistant", "content": [{"type": "text", "text": "issued"}]}]
        self.ledger.register_response(source, "openai", "checkpoint-source", request_prefix, request_prefix + issued, "sha256:issued", None, b'{"output":"issued"}')
        metadata = {"thread_id": "detached", "session_id": "operator", "forked_from_thread_id": "source"}
        payload = {"client_metadata": {"x-codex-turn-metadata": metadata}, "input": [
            {"type": "message", "role": "user", "content": "source"},
            {"type": "message", "role": "assistant", "content": "issued"},
            {"type": "message", "role": "user", "content": "fresh"},
        ]}
        context = mediator.open({"X-Codex-Turn-Metadata": json.dumps(metadata)}, payload, "openai")
        self.assertEqual(context.kind, "fork")
        self.assertEqual(checkpoints.forks, [("checkpoint-source", "codex:thread:detached")])
        self.assertEqual(self.ledger.root_for(context.trajectory_id), context.trajectory_id)

    def test_request_only_or_changed_assistant_history_cannot_match_checkpoint(self):
        source, scope = "codex:thread:source-negative", "codex:scope:operator"
        self.ledger.ensure_root(source, "codex", scope)
        self.ledger.record_checkpoint(source, "checkpoint-negative")
        request = [{"type": "message", "role": "user", "content": [{"type": "text", "text": "source"}]}]
        issued = [{"type": "message", "role": "assistant", "content": [{"type": "text", "text": "PUBLIC"}]}]
        self.ledger.register_response(source, "openai", "checkpoint-negative", request, request + issued, "sha256:issued", None, b"issued")
        self.assertIsNone(self.ledger.matching_response_binding("openai", request + [{"type": "message", "role": "user", "content": [{"type": "text", "text": "fresh"}]}], None))
        private = [{"type": "message", "role": "assistant", "content": [{"type": "text", "text": "PRIVATE"}]}]
        self.assertIsNone(self.ledger.matching_response_binding("openai", request + private + [{"type": "message", "role": "user", "content": [{"type": "text", "text": "fresh"}]}], None))

    def test_response_binding_upgrade_preserves_contexts_and_rejects_ambiguous_fork_history(self):
        self.ledger.close()
        self.db.unlink()
        self.db.with_name(self.db.name + "-shm").unlink(missing_ok=True)
        self.db.with_name(self.db.name + "-wal").unlink(missing_ok=True)
        connection = sqlite3.connect(self.db)
        connection.executescript(
            """
            CREATE TABLE trajectories (
                trajectory_id TEXT PRIMARY KEY, client TEXT NOT NULL, principal_scope TEXT NOT NULL,
                kind TEXT NOT NULL, parent_trajectory TEXT, parent_call_id TEXT, current_anchor TEXT NOT NULL
            );
            CREATE TABLE checkpoints (checkpoint_id TEXT PRIMARY KEY, trajectory_id TEXT NOT NULL, anchor TEXT NOT NULL);
            CREATE TABLE lifecycle_events (
                trajectory_id TEXT NOT NULL, event TEXT NOT NULL, fingerprint TEXT NOT NULL, status TEXT NOT NULL,
                payload TEXT, PRIMARY KEY(trajectory_id, event, fingerprint)
            );
            CREATE TABLE response_bindings (
                source_trajectory TEXT NOT NULL, provider TEXT NOT NULL, checkpoint_id TEXT NOT NULL,
                inherited_prefix TEXT NOT NULL, request_prefix TEXT NOT NULL, issued_item_digest TEXT NOT NULL,
                bootstrap_digest TEXT, source_scope TEXT NOT NULL, actual_message_bytes BLOB,
                actual_message_hash TEXT NOT NULL, terminal_omission INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (source_trajectory, provider, checkpoint_id),
                UNIQUE (provider, issued_item_digest, bootstrap_digest)
            );
            """
        )
        request = [{"type": "message", "role": "user", "content": [{"type": "text", "text": "source"}]}]
        issued = [{"type": "message", "role": "assistant", "content": [{"type": "text", "text": "PUBLIC"}]}]
        inherited = json.dumps(request + issued, separators=(",", ":"))
        request_json = json.dumps(request, separators=(",", ":"))
        actual = b"legacy public response"
        actual_hash = "sha256:" + hashlib.sha256(actual).hexdigest()
        for source, checkpoint in (("codex:thread:source-a", "checkpoint-a"), ("codex:thread:source-b", "checkpoint-b")):
            connection.execute(
                "INSERT INTO trajectories VALUES (?, 'codex', 'codex:scope:operator', 'root', NULL, NULL, 'anchor')",
                (source,),
            )
            connection.execute("INSERT INTO checkpoints VALUES (?, ?, 'anchor')", (checkpoint, source))
            connection.execute(
                "INSERT INTO response_bindings VALUES (?, 'openai', ?, ?, ?, 'sha256:public', NULL, "
                "'codex:scope:operator', ?, ?, 0)",
                (source, checkpoint, inherited, request_json, actual, actual_hash),
            )
        connection.execute("INSERT INTO lifecycle_events VALUES ('codex:thread:source-a', 'journal', 'record', 'done', '{}')")
        connection.commit()
        connection.close()

        self.ledger = LifecycleLedger(self.db, self.key)
        self.assertEqual(self.ledger._connection.execute("SELECT COUNT(*) FROM response_bindings").fetchone()[0], 2)
        self.assertEqual(self.ledger._connection.execute("SELECT COUNT(*) FROM lifecycle_events").fetchone()[0], 1)
        self.assertEqual(
            self.ledger._connection.execute(
                "SELECT source_rows FROM lifecycle_ledger_migrations WHERE migration = 'response_bindings_context_key_v2'"
            ).fetchone()[0],
            2,
        )

        self.ledger.register_response(
            "codex:thread:source-a", "openai", "checkpoint-a", request, request + issued,
            "sha256:public", None, actual,
        )
        self.assertEqual(self.ledger._connection.execute("SELECT COUNT(*) FROM response_bindings").fetchone()[0], 2)
        for source, checkpoint in (("codex:thread:source-c", "checkpoint-c"), ("codex:thread:source-d", "checkpoint-d")):
            self.ledger.ensure_root(source, "codex", "codex:scope:operator")
            self.ledger.record_checkpoint(source, checkpoint)
            self.ledger.register_response(
                source, "openai", checkpoint, request, request + issued, "sha256:public",
                "sha256:bootstrap", b"new public response",
            )
        self.assertEqual(self.ledger._connection.execute("SELECT COUNT(*) FROM response_bindings").fetchone()[0], 4)
        fresh = {"type": "message", "role": "user", "content": [{"type": "text", "text": "fresh"}]}
        with self.assertRaisesRegex(LifecycleLedgerError, "ambiguously matches"):
            self.ledger.matching_response_binding("openai", request + issued + [fresh], None)

    def test_unknown_prior_assistant_history_cannot_seed_a_native_root(self):
        metadata = {"thread_id": "new", "session_id": "operator"}
        payload = {"client_metadata": {"x-codex-turn-metadata": metadata}, "input": [
            {"type": "message", "role": "assistant", "content": "unregistered"},
            {"type": "message", "role": "user", "content": "fresh"},
        ]}
        with self.assertRaisesRegex(GateMediationError, "lacks an exact checkpointed"):
            self.mediator.open({"X-Codex-Turn-Metadata": json.dumps(metadata)}, payload, "openai")

    def test_observed_opaque_items_are_trajectory_owned(self):
        item = {"type": "reasoning", "encrypted_content_digest": "sha256:opaque"}
        self.ledger.ensure_root("codex:thread:opaque-a", "codex", "codex:scope:a")
        self.ledger.ensure_root("codex:thread:opaque-b", "codex", "codex:scope:b")
        self.ledger.record_observed_opaque("codex:thread:opaque-a", "openai", [item])
        self.assertTrue(self.ledger.known_opaque_history("codex:thread:opaque-a", "openai", [item]))
        self.assertFalse(self.ledger.known_opaque_history("codex:thread:opaque-b", "openai", [item]))

    def test_unknown_opaque_compaction_history_fails_closed(self):
        metadata = {"thread_id": "root", "session_id": "operator", "compaction": {"kind": "compaction"}}
        root = self.mediator.open({"X-Codex-Turn-Metadata": json.dumps({"thread_id": "root", "session_id": "operator"})}, {"client_metadata": {"x-codex-turn-metadata": {"thread_id": "root", "session_id": "operator"}}, "input": []}, "openai")
        payload = {"client_metadata": {"x-codex-turn-metadata": metadata}, "input": [{"type": "compaction", "encrypted_content": "unknown"}]}
        with self.assertRaisesRegex(GateMediationError, "unknown opaque"):
            self.mediator.open({"X-Codex-Turn-Metadata": json.dumps(metadata)}, payload, "openai")



if __name__ == "__main__":
    unittest.main(verbosity=2)
