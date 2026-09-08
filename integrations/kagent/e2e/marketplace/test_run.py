"""Non-vacuous evidence checks; these are not cluster acceptance."""
from copy import deepcopy
import importlib.util
import json
from pathlib import Path

import pytest

spec = importlib.util.spec_from_file_location("marketplace_acceptance", Path(__file__).with_name("run.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def test_refusal_requires_complete_script_and_real_policy_feedback():
    denied = {"role": "tool", "content": json.dumps({"appa": "denied", "result": "blocked"})}
    state = {"requests": [{"index": 0, "messages": []}, {"index": 1, "messages": [denied]}], "invocations": []}
    runner.assert_evidence(state, 2, 0, 0, 1)
    for broken in (dict(state, requests=[]), dict(state, requests=state["requests"][:1]), dict(state, invocations=[{"tool": "issue_write"}])):
        with pytest.raises(RuntimeError):
            runner.assert_evidence(broken, 2, 0, 0, 1)
    broken = deepcopy(state)
    broken["requests"][-1]["messages"] = [{"role": "assistant", "content": "The tool was denied"}]
    with pytest.raises(RuntimeError):
        runner.assert_evidence(broken, 2, 0, 0, 1)


def test_nested_mcp_json_refusal_is_counted_once():
    feedback = {"content": [{"text": json.dumps({"appa": "denied", "result": "blocked"})}]}
    assert runner.denial_count(feedback) == 1
    assert runner.denial_count("the model claims it was denied") == 0
