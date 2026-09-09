"""Offline regressions for the live matrix's evidence checks."""

import importlib.util
import io
import json
from pathlib import Path
import threading

import pytest


@pytest.fixture
def helpers(monkeypatch):
    monkeypatch.setenv("APPA_A2A_E2E", "1")
    spec = importlib.util.spec_from_file_location("appa_a2a_helpers", Path(__file__).parent / "a2a/conftest.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_success_prose_is_not_a_tool_result(helpers):
    task = helpers.Task({"artifacts": [{"parts": [{"kind": "text", "text": "Rollback completed"}]}]})
    assert not task.has_result("rollback_deployment", rolled_back="checkout-api")


def test_nested_mcp_result_is_observed_but_call_arguments_are_not(helpers):
    data = {"name": "rollback_deployment", "args": {"rolled_back": "checkout-api"}}
    task = helpers.Task({"history": [{"role": "agent", "parts": [{"kind": "data", "data": data}]}]})
    assert not task.has_result("rollback_deployment", rolled_back="checkout-api")
    data["response"] = {"content": [{"text": json.dumps({"rolled_back": "checkout-api"})}]}
    assert task.has_result("rollback_deployment", rolled_back="checkout-api")


def test_board_matches_canonical_mcp_names_not_other_tool_families(helpers, monkeypatch):
    entries = [
        {"id": str(i), "tool": tool}
        for i, tool in enumerate(
            [
                "rollback_deployment",
                "mcp/demo/rollback_deployment",
                "agent/demo/rollback_deployment",
                "mcp/demo/rollback_deployment_extra",
            ]
        )
    ]
    monkeypatch.setattr(
        helpers.urllib.request,
        "urlopen",
        lambda *a, **k: io.BytesIO(json.dumps({"pending": entries}).encode()),
    )
    assert [item["id"] for item in helpers.Board("http://mock").pending("rollback_deployment")] == ["0", "1"]


@pytest.mark.parametrize("acknowledged", [True, False])
def test_board_requires_matching_ruling_acknowledgement(helpers, monkeypatch, acknowledged):
    board = helpers.Board("http://mock")
    monkeypatch.setattr(
        board,
        "pending",
        lambda tool: [{"id": "pending-1", "tool": "mcp/demo/rollback_deployment"}],
    )
    stop = threading.Event()

    def decide(request, **kwargs):
        assert json.loads(request.data)["id"] == "pending-1"
        stop.set()
        return io.BytesIO(json.dumps({"decided": "pending-1" if acknowledged else None}).encode())

    monkeypatch.setattr(helpers.urllib.request, "urlopen", decide)
    result = board.rule("rollback_deployment", "approve", stop=stop)
    assert bool(result) is acknowledged


def test_missing_ruling_fails_the_scenario(helpers, monkeypatch):
    board = helpers.Board("http://mock")
    monkeypatch.setattr(board, "rule", lambda *a, **k: None)
    with pytest.raises(AssertionError, match="acknowledged an actual"):
        with board.ruling("rollback_deployment", "approve"):
            pass


def test_board_member_stops_when_the_agent_fails(helpers, monkeypatch):
    board = helpers.Board("http://mock")
    stopped = threading.Event()

    def rule(*args, stop):
        stop.wait(2)
        stopped.set()

    monkeypatch.setattr(board, "rule", rule)
    with pytest.raises(ValueError, match="agent failed"):
        with board.ruling("rollback_deployment", "approve"):
            raise ValueError("agent failed")
    assert stopped.is_set()
