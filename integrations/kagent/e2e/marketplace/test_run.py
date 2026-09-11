"""Non-vacuous evidence checks; these are not cluster acceptance."""
from copy import deepcopy
import importlib.util
import json
import os
from pathlib import Path
import sys

import pytest

spec = importlib.util.spec_from_file_location("marketplace_acceptance", Path(__file__).with_name("run.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def test_refusal_requires_complete_script_and_real_policy_feedback():
    denied = {"role": "tool", "content": json.dumps({"appa": "denied", "result": "blocked"})}
    state = {"requests": [{"index": 0, "messages": []}, {"index": 1, "messages": [denied]}], "invocations": [], "lookups": []}
    runner.assert_evidence(state, 2, 0, 0, 1, 0)
    for broken in (dict(state, requests=[]), dict(state, requests=state["requests"][:1]), dict(state, invocations=[{"tool": "issue_write"}])):
        with pytest.raises(RuntimeError):
            runner.assert_evidence(broken, 2, 0, 0, 1, 0)
    broken = deepcopy(state)
    broken["requests"][-1]["messages"] = [{"role": "assistant", "content": "The tool was denied"}]
    with pytest.raises(RuntimeError):
        runner.assert_evidence(broken, 2, 0, 0, 1, 0)


def test_nested_mcp_json_refusal_is_counted_once():
    feedback = {"content": [{"text": json.dumps({"appa": "denied", "result": "blocked"})}]}
    assert runner.denial_count(feedback) == 1
    assert runner.denial_count("the model claims it was denied") == 0


def test_native_adk_text_refusal_requires_tool_feedback_and_no_mcp_call():
    blocked = ("[appa] Blocked: this call cannot run yet.\n\nWhy:\n"
               "  - its result comes with a restriction this session does not have yet. If you take the result in, the restriction applies to everything this session produces afterwards, and tools that send or publish check it:\n"
               "      trust: what you produce is treated as suspicious (now: trusted)")
    state = {"requests": [{"index": 0, "messages": []},
                          {"index": 1, "messages": [{"role": "tool", "content": blocked}]}],
             "invocations": [], "lookups": []}
    runner.assert_evidence(state, 2, 0, 0, 1, 0)
    for role, content in (("assistant", blocked), ("tool", "connection refused"),
                          ("tool", "the model claims: " + blocked)):
        broken = deepcopy(state)
        broken["requests"][-1]["messages"] = [{"role": role, "content": content}]
        with pytest.raises(RuntimeError):
            runner.assert_evidence(broken, 2, 0, 0, 1, 0)
    with pytest.raises(RuntimeError):
        runner.assert_evidence(dict(state, invocations=[{"tool": "get_file_contents"}]), 2, 0, 0, 1, 0)
    with pytest.raises(RuntimeError):
        runner.assert_evidence(state, 2, 0, 0, 1, 1)


def test_missing_container_logs_do_not_skip_other_diagnostics_or_cleanup(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    calls = tmp_path / "calls.jsonl"
    tool = f"#!{sys.executable}\n" + """
import json, os, pathlib, sys
with open(os.environ['ACCEPTANCE_TEST_CALLS'], 'a') as output:
    output.write(json.dumps([pathlib.Path(sys.argv[0]).name, *sys.argv[1:]]) + '\\n')
sys.exit(1 if 'logs' in sys.argv else 0)
"""
    for name in ("kubectl", "kind", "docker"):
        executable = bin_dir / name
        executable.write_text(tool)
        executable.chmod(0o700)
    acceptance = runner.Acceptance(tmp_path / "work")
    acceptance.env.update(PATH=str(bin_dir) + os.pathsep + os.environ['PATH'],
                          ACCEPTANCE_TEST_CALLS=str(calls))
    acceptance.cluster_created = acceptance.registry_created = True
    acceptance.close()
    commands = [json.loads(line) for line in calls.read_text().splitlines()]
    assert any('events' in command for command in commands)
    assert sum('describe' in command for command in commands) == 2
    assert sum('logs' in command for command in commands) == 2
    assert commands[-2] == ['kind', 'delete', 'cluster', '--name', acceptance.name]
    assert commands[-1] == ['docker', 'rm', '-f', acceptance.registry]
