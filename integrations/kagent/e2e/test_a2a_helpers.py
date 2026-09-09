"""Offline regressions for the live matrix's evidence checks."""

import ast
import importlib.util
import io
import json
import textwrap
import threading
from pathlib import Path
from types import SimpleNamespace

import pytest


def test_ui_results_are_scoped_and_reading_twice_does_not_collapse_them():
    source = Path(__file__).parent / "ui/conftest.py"
    tree = ast.parse(source.read_text())
    chat_class = next(node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == "Chat")
    namespace = {"REPLY_TIMEOUT_S": 300, "json": json}
    exec(compile(ast.Module(body=[chat_class], type_ignores=[]), str(source), "exec"), namespace)  # noqa: S102 -- repository test code only

    class Output:
        def __init__(self):
            self.expanded = False
            self.first = self

        def count(self):
            return int(self.expanded)

        def wait_for(self, **kwargs):
            assert self.expanded

        def all_inner_texts(self):
            return ['{"rolled_back": "checkout-api"}']

    class Button:
        def __init__(self):
            self.output = Output()
            self.clicks = 0

        def is_visible(self):
            return True

        def click(self):
            self.clicks += 1
            self.output.expanded = not self.output.expanded

        def locator(self, selector):
            if selector == "xpath=..":
                return SimpleNamespace(locator=lambda name: self.output if name == "pre" else None)
            assert "ancestor::div" in selector and "min-w-full" in selector
            return SimpleNamespace(locator=lambda name: SimpleNamespace(
                first=SimpleNamespace(inner_text=lambda: "rollback_deployment"),
            ))

    button = Button()

    def get_by_role(role, *, name, exact):
        assert role == "button" and exact
        return SimpleNamespace(all=lambda: [button] if name == "Results" else [])

    # Deliberately no page.inner_text: whole-page prose/arguments cannot enter.
    chat = namespace["Chat"](SimpleNamespace(get_by_role=get_by_role))
    assert chat.has_result("rollback_deployment", rolled_back="checkout-api")
    assert chat.has_result("rollback_deployment", rolled_back="checkout-api")
    assert not chat.has_result("restart_deployment", rolled_back="checkout-api")
    assert chat.tool_results() == '{"rolled_back": "checkout-api"}'
    assert button.clicks == 1


def test_scripted_parent_instructions_match_the_shipped_demo():
    kagent = Path(__file__).resolve().parents[1]
    tree = ast.parse((kagent / "tests/conftest.py").read_text())
    instruction = next(
        ast.literal_eval(node.value)
        for node in tree.body
        if isinstance(node, ast.Assign)
        and any(isinstance(target, ast.Name) and target.id == "PARENT_INSTRUCTION" for target in node.targets)
    )
    chart = (kagent / "demo/chart/templates/agents.yaml").read_text()
    parent = chart.split("  name: cluster-ops\n", 1)[1]
    message = parent.split("    systemMessage: |\n", 1)[1].split("    modelConfig:", 1)[0]
    assert instruction == textwrap.dedent(message)


@pytest.mark.parametrize("failure", [None, "rollout", "request"])
def test_protocol_clone_preserves_configuration_and_cleans_up(helpers, monkeypatch, failure):
    commands = []
    created = []
    stopped = []
    monkeypatch.setenv("APPA_E2E_AGENT", "cluster-ops-go")

    def run(command, **kwargs):
        args = command[3:]
        commands.append(args)
        if args[:2] == ["get", "agent"]:
            assert args[2] == "cluster-ops-go"
            return SimpleNamespace(stdout=json.dumps({
                "apiVersion": "kagent.dev/v1alpha2",
                "spec": {"declarative": {"modelConfig": "real-model", "tools": [{"name": "write"}]}},
            }))
        if args[0] == "create":
            created.append(json.loads(kwargs["input"]))
        if args[0] == "rollout" and failure == "rollout":
            raise RuntimeError("rollout failed")
        return SimpleNamespace(stdout='{"ready": true}')

    def popen(command, **kwargs):
        assert command[4] == "svc/" + created[0]["metadata"]["name"]
        kwargs["stdout"].write("Forwarding from 127.0.0.1:32123 -> 8080\n")
        return SimpleNamespace(terminate=lambda: stopped.append(True), wait=lambda **kw: 0, poll=lambda: None)

    monkeypatch.setattr(helpers.subprocess, "run", run)
    monkeypatch.setattr(helpers.subprocess, "Popen", popen)
    fixture = helpers.protocol_agent.__wrapped__()
    if failure == "rollout":
        with pytest.raises(RuntimeError, match="rollout failed"):
            next(fixture)
    else:
        agent = next(fixture)
        assert agent.url == "http://127.0.0.1:32123/"
        if failure == "request":
            with pytest.raises(RuntimeError, match="request failed"):
                fixture.throw(RuntimeError("request failed"))
        else:
            with pytest.raises(StopIteration):
                next(fixture)
        assert stopped == [True]
    declaration = created[0]["spec"]["declarative"]
    assert declaration["modelConfig"] == "real-model"
    assert declaration["tools"] == []
    assert "negative test" in declaration["systemMessage"]
    assert commands[-1] == ["delete", "agent", created[0]["metadata"]["name"], "--wait=false", "--ignore-not-found"]


@pytest.fixture(params=["a2a", "ui"])
def helpers(monkeypatch, request):
    monkeypatch.setenv("APPA_A2A_E2E", "1")
    spec = importlib.util.spec_from_file_location("appa_a2a_helpers", Path(__file__).parent / "a2a/conftest.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    if request.param == "ui":
        # Exercise the UI board helper without importing or launching a browser.
        source = Path(__file__).parent / "ui/conftest.py"
        tree = ast.parse(source.read_text())
        board = next(node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == "Board")
        exec(compile(ast.Module(body=[board], type_ignores=[]), str(source), "exec"), module.__dict__)  # noqa: S102 -- repository test code only
    return module


def test_success_prose_is_not_a_tool_result(helpers):
    task = helpers.Task({"artifacts": [{"parts": [{"kind": "text", "text": "Rollback completed"}]}]})
    assert not task.has_result("rollback_deployment", rolled_back="checkout-api")


def test_tool_evidence_rejects_prefix_collisions(helpers):
    names = ["rollback_deployment_extra", "rollback_deployment_go", "agent/demo/rollback_deployment"]
    task = helpers.Task({"history": [{"parts": [
        {"kind": "data", "data": {"name": name, "args": {}, "response": {"rolled_back": "checkout-api"}}}
        for name in names
    ]}]})
    assert not task.calls("rollback_deployment")
    assert not task.responses("rollback_deployment")
    assert not task.has_result("rollback_deployment", rolled_back="checkout-api")


def test_agent_evidence_accepts_only_exact_name_or_go_variant(helpers):
    tool = "kagent__NS__log_analyst"
    task = helpers.Task({"history": [{"parts": [
        {"kind": "data", "data": {"name": name, "args": {}, "response": {"result": name}}}
        for name in [tool, tool + "_go", tool + "_extra", tool + "_go_extra"]
    ]}]})
    assert [call["name"] for call in task.calls(tool)] == [tool, tool + "_go"]
    assert [body["result"] for body in task.responses(tool)] == [tool, tool + "_go"]


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
    with pytest.raises(AssertionError, match="acknowledged an actual"), board.ruling("rollback_deployment", "approve"):
        pass


def test_board_member_stops_when_the_agent_fails(helpers, monkeypatch):
    board = helpers.Board("http://mock")
    stopped = threading.Event()

    def rule(*args, stop):
        stop.wait(2)
        stopped.set()

    monkeypatch.setattr(board, "rule", rule)
    with pytest.raises(ValueError, match="agent failed"), board.ruling("rollback_deployment", "approve"):
        raise ValueError("agent failed")
    assert stopped.is_set()
