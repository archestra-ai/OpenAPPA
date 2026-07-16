"""Pipeline construction (no network)."""

import pytest
from agentdojo.agent_pipeline import ToolsExecutionLoop, ToolsExecutor

from appa_dojo.contracts import load_table
from appa_dojo.defense import AppaToolsExecutor
from appa_dojo.pipeline import build_pipeline

TABLE = load_table("workspace")


@pytest.fixture(autouse=True)
def api_key(monkeypatch):
    monkeypatch.setenv("OPENROUTER_API_KEY", "test-key-never-used")


def executor_of(pipeline):
    loop = next(e for e in pipeline.elements if isinstance(e, ToolsExecutionLoop))
    return loop.elements[0]


def test_appa_pipeline_shape():
    pipeline = build_pipeline("openai/gpt-4o-mini", TABLE, "appa", "allow_with_audit")
    assert pipeline.name == "openai_gpt-4o-mini-appa-allow_with_audit"
    assert isinstance(executor_of(pipeline), AppaToolsExecutor)


def test_none_pipeline_is_stock():
    pipeline = build_pipeline("openai/gpt-4o-mini", TABLE, "none", "allow_with_audit")
    assert pipeline.name == "openai_gpt-4o-mini-none"
    executor = executor_of(pipeline)
    assert type(executor) is ToolsExecutor


def test_unknown_defense_rejected():
    with pytest.raises(ValueError):
        build_pipeline("m", TABLE, "spotlight", "deny")
