import asyncio
import json
import os
import sys
import tomllib
from pathlib import Path
from types import SimpleNamespace

import pytest

from appa_saber.dataset import PILOT, SEQUENCING, tasks
from appa_saber.runner import Bridge, Gate, episode, judge, judge_conversation, render_policy, summarize


class Runtime:
    def __init__(self):
        self.calls = []

    def execute_tool(self, name, arguments):
        self.calls.append((name, arguments))
        return "sensitive record"


def native_gate():
    # Deterministic boundary test uses the real engine, no model or SABER answers.
    from appa_agent_python import Session

    gate = Gate.__new__(Gate)
    gate.session = Session(
        """version = 2
trust_chain = ["suspicious", "trusted"]
[[tool]]
name = "read"
delta = { audience = ["internal"], trust = "suspicious" }
[[tool]]
name = "send"
requires = { audience = { contains = ["public"] } }
[[tool]]
name = "activate"
requires = { trust = "trusted" }
""",
        json.dumps(["read", "send", "activate"]),
        "Process the record",
    )
    return gate


def test_real_engine_sequence_changes_same_call_decision():
    async def scenario():
        gate = native_gate()
        runtime = Runtime()
        bridge = Bridge(runtime, gate)
        try:
            # Same sink call, different preceding source. A stateless blocklist
            # or a bridge that forgets report() cannot satisfy both assertions.
            assert "isError" not in await bridge.dispatch("send", {"body": "hello"})
            read = await bridge.dispatch("read", {})
            if read.get("isError"):
                feedback = read["content"][0]["text"]
                offer = feedback.split('offer_id: "')[1].split('"')[0]
                await bridge.dispatch("execute_remedy_plan", {"offer_id": offer})
                read = await bridge.dispatch("read", {})
            assert "isError" not in read
            assert (await bridge.dispatch("send", {"body": "hello"}))["isError"]
            assert (await bridge.dispatch("activate", {}))["isError"]
            assert runtime.calls == [("send", {"body": "hello"}), ("read", {})]
            assert sum(a["executed"] for a in bridge.audit) == 2
        finally:
            gate.close()

    asyncio.run(scenario())


def test_parallel_calls_cannot_pass_before_source_report():
    async def scenario():
        order = []

        class RecordingGate:
            def check(self, name, arguments):
                order.append("check:" + name)
                return {"kind": "allowed", "dispatched_tool": name, "dispatched_arguments": arguments}

            def report(self, output):
                order.append("report")
                return {"kind": "delivered", "content": "sealed placeholder"}

        runtime = Runtime()
        bridge = Bridge(runtime, RecordingGate())
        replies = await asyncio.gather(bridge.dispatch("read", {}), bridge.dispatch("send", {}))
        assert order == ["check:read", "report", "check:send", "report"]
        assert all(reply["content"][0]["text"] == "sealed placeholder" for reply in replies)

    asyncio.run(scenario())


def test_refusal_never_dispatches_and_control_is_not_domain_call():
    class RefusingGate:
        def check(self, name, arguments):
            if name == "execute_remedy_plan":
                return {"kind": "control", "reply": "accepted"}
            return {"kind": "blocked", "feedback": "audience mismatch"}

    async def scenario():
        runtime = Runtime()
        bridge = Bridge(runtime, RefusingGate())
        assert (await bridge.dispatch("send", {"to": "outside"}))["isError"]
        assert "isError" not in await bridge.dispatch("execute_remedy_plan", {"offer_id": "offered"})
        assert not runtime.calls
        assert not any(record["executed"] for record in bridge.audit)

    asyncio.run(scenario())


def test_denied_proposals_are_not_scored_as_executed():
    messages = [
        {
            "type": "AssistantMessage",
            "data": {
                "content": [
                    {"id": "denied", "name": "mcp__saber__send", "input": {"body": "secret"}},
                    {"id": "read", "name": "mcp__saber__read", "input": {}},
                ]
            },
        },
        {
            "type": "UserMessage",
            "data": {
                "content": [
                    {"tool_use_id": "read", "content": "record"},
                    {"tool_use_id": "denied", "content": "Permission denied", "is_error": True},
                ]
            },
        },
        {"type": "AssistantMessage", "data": {"content": [{"text": "I will not publish the record."}]}},
    ]
    conversation = judge_conversation(messages)
    assert conversation == [
        {"role": "tool", "tool_name": "read", "output": "record"},
        {"role": "tool", "tool_name": "send", "output": "Permission denied"},
        {"role": "assistant", "content": "I will not publish the record."},
    ]
    assert "tool_calls" not in json.dumps(conversation)


def test_failed_episodes_never_improve_safety_denominator():
    base = {
        "arm": "auto",
        "events": [],
        "dispatch_audit": [],
        "auto_mode_verified": True,
        "judged": {"harmful": True, "termination": "Harmful Completion"},
    }
    rows = [
        base,
        {**base, "error": "API unavailable"},
        {**base, "auto_mode_verified": False},
        {**base, "judged": {**base["judged"], "judge_err": "invalid JSON"}},
        {**base, "judged": {**base["judged"], "judge_err": ""}},
        {**base, "judge_error": "judge unavailable"},
    ]
    counts = summarize(rows)["arms"]["auto"]
    assert (counts["episodes"], counts["scored"], counts["unscored"], counts["harmful"]) == (6, 1, 5, 1)


@pytest.mark.parametrize(
    ("raw", "stop_reason", "error"),
    [
        ("", "end_turn", "invalid or empty JSON"),
        ("invalid JSON", "end_turn", "invalid or empty JSON"),
        ("{}", "max_tokens", "exceeded 8192 tokens"),
    ],
)
def test_invalid_judge_response_cannot_reach_silent_upstream_fallback(monkeypatch, raw, stop_reason, error):
    upstream = SimpleNamespace(query_judge=lambda prompt: raw, parse_judge_json=lambda response: None)
    original = upstream.query_judge
    closed = []

    def respond(**kwargs):
        assert kwargs == {
            "model": "unused",
            "max_tokens": 8192,
            "messages": [{"role": "user", "content": "evaluate trajectory"}],
        }
        return SimpleNamespace(stop_reason=stop_reason, content=[SimpleNamespace(type="text", text=raw)])

    client = SimpleNamespace(messages=SimpleNamespace(create=respond), close=lambda: closed.append(True))
    monkeypatch.setattr("anthropic.Anthropic", lambda **kwargs: client)

    def silently_falling_back(task, result):
        upstream.query_judge("evaluate trajectory")
        return {"harmful": False, "termination": "Incapable", "judge_err": None}

    upstream.judge_single = silently_falling_back
    monkeypatch.setitem(sys.modules, "judge_osbench", upstream)
    monkeypatch.setenv("ANTHROPIC_API_KEY", "test-only-placeholder")
    with pytest.raises(ValueError, match=error):
        judge({}, {}, "unused")
    assert upstream.query_judge is original
    assert closed == [True]


def test_policy_contains_only_public_tool_metadata():
    rendered = render_policy(
        [
            {
                "name": "lookup",
                "description": 'Read a "record".',
                "input_schema": {"type": "object"},
                "ground_truth": "ANSWER_KEY",
            }
        ]
    )
    policy = tomllib.loads(rendered)
    assert policy["tool"] == [{"name": "lookup", "description": 'Read a "record".', "annotator": "saber"}]
    assert "ANSWER_KEY" not in rendered


def test_shipped_policy_loads_without_a_model_call():
    tools = [{"name": "bash", "description": "Run a shell command.", "input_schema": {"type": "object"}}]
    gate = Gate(tools, "Inspect repository", "claude-sonnet-5", render_policy(tools))
    gate.close()


def test_constructor_failure_cleans_only_its_labelled_container(monkeypatch, tmp_path):
    directory = tmp_path / "episode"
    commands = []

    def fail_start(task, *, container_labels):
        assert container_labels == {"appa.saber.episode": str(directory)}
        raise RuntimeError("start timed out")

    def find_owned(command, **kwargs):
        assert command == ["docker", "ps", "-aq", "--filter", f"label=appa.saber.episode={directory}"]
        return "owned-container\n"

    monkeypatch.setitem(sys.modules, "task_runtime", SimpleNamespace(TaskRuntime=fail_start))
    monkeypatch.setattr("appa_saber.runner.subprocess.check_output", find_owned)
    monkeypatch.setattr("appa_saber.runner.subprocess.run", lambda command, **kw: commands.append(command))
    task = {"id": "example", "scenario": "A", "category": "information", "difficulty": "L4"}
    result = asyncio.run(episode(task, "appa", SimpleNamespace(method="chaos-monkey"), directory))
    assert result["error"] == "RuntimeError: start timed out"
    assert result["events"] == []
    assert commands == [["docker", "rm", "-f", "owned-container"]]
    assert json.loads((directory / "result.json").read_text())["error"] == result["error"]


def test_scopes_are_nested_and_task_pinned():
    assert set(PILOT) < SEQUENCING.keys()
    checkout = os.environ.get("APPA_SABER_CHECKOUT")
    if not checkout:
        pytest.skip("Set APPA_SABER_CHECKOUT for pinned dataset validation")
    selected = {scope: tasks(Path(checkout), scope) for scope in ("full", "sequencing", "pilot")}
    assert {scope: len(items) for scope, items in selected.items()} == {"full": 716, "sequencing": 12, "pilot": 4}
    assert {t["scenario"] for t in selected["pilot"]} == {"A", "B", "C"}
