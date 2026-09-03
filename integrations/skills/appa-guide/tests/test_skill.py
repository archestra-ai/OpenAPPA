from __future__ import annotations

from pathlib import Path

import tomllib

REPO = Path(__file__).resolve().parents[4]
SKILL_DIR = Path(__file__).resolve().parents[1]
ROUTER = SKILL_DIR / "SKILL.md"
KAGENT = SKILL_DIR / "references" / "kagent.md"
CLAUDE = SKILL_DIR / "references" / "claude-code.md"
CHART = REPO / "integrations/kagent/demo/chart"
GUIDE_YAML = CHART / "templates/guide.yaml"
DEMO_POLICY = CHART / "files/demo.appa.toml"


def test_router_names_both_references_and_stays_host_neutral():
    text = ROUTER.read_text(encoding="utf-8")
    assert text.startswith("---\n")
    assert "name: appa-guide" in text
    assert "references/kagent.md" in text
    assert "references/claude-code.md" in text
    assert "k8s_get_resources" not in text.split("## Detect the host")[0]
    assert "claude mcp list" not in text


def test_claude_reference_delegates_to_the_installed_plugin_skill():
    text = CLAUDE.read_text(encoding="utf-8")
    assert "/appa-guide" in text
    assert "appa init claude-code" in text


def test_kagent_reference_carries_the_flow():
    text = KAGENT.read_text(encoding="utf-8")
    assert "`init`" in text and "`adjust`" in text
    assert "status.discoveredTools" in text
    assert "__NS__" in text
    assert "k8s_apply_manifest" in text
    assert "Approve/Reject card" in text
    assert "/reload" in text
    assert "kubelet syncs" in text
    assert "Read-only fallback" in text
    assert "Approve, or tell me what to change." in text
    assert "OpenAPPA pieces" in text
    assert "human-approval" in text


def test_kagent_reference_has_no_claude_code_machinery():
    text = KAGENT.read_text(encoding="utf-8")
    for leftover in ("claude mcp list", "marketplace-root", "clappa", "APPA_GATE"):
        assert leftover not in text, leftover
    # The guide never configures itself or uses bash.
    assert "appa-guide` and" in text
    assert "Never kubectl, never bash" in text


def test_chart_agent_wires_skill_tools_and_shared_runtime():
    text = GUIDE_YAML.read_text(encoding="utf-8")
    assert "kind: Agent" in text
    assert "name: appa-guide" in text
    assert "gitRefs" in text
    assert "integrations/skills/appa-guide" in text
    for tool in ("k8s_get_resources", "k8s_get_resource_yaml", "k8s_apply_manifest", "k8s_execute_command"):
        assert tool in text, tool
    assert "APPA_RUNTIME_URL" in text
    assert "read_file" in text  # the reference file the router tells the agent to read
    # The superseded packaging is gone: no custom MCP server, no embedded runtime.
    assert "guide-tools" not in text
    assert "APPA_CONFIG" not in text
    assert "kind: Deployment" not in text
    assert "kind: Role" not in text


def test_demo_policy_gates_the_guide_write_path():
    parsed = tomllib.loads(DEMO_POLICY.read_text(encoding="utf-8"))
    tools = {tool["name"]: tool for tool in parsed["policy"]["tool"]}
    assert "delta" in tools["k8s_get_resources"]
    assert "delta" in tools["k8s_get_resource_yaml"]
    apply = tools["k8s_apply_manifest"]
    assert apply["requires"]["attention"] == ["human-approval"]
    exec_ = tools["k8s_execute_command"]
    assert exec_["requires"]["trust"] == "trusted"
    # The skill's own tools read only; the file-write helpers stay undeclared.
    assert "delta" in tools["appa-guide"]
    assert "delta" in tools["read_file"]
    assert "bash" not in tools
    assert "write_file" not in tools
    authorities = {item["name"] for item in parsed["policy"]["authority"]}
    assert "oncall" in authorities


def test_old_custom_mcp_guide_is_gone():
    assert not (REPO / "integrations/kagent/guide").exists()
