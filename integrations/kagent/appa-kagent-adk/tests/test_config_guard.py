"""The refusal set, against a local schema and the real AgentConfig."""

import pydantic
import pytest

from appa_kagent_adk.config_guard import ConfigRefused, refuse_unsupported


class Inner(pydantic.BaseModel):
    url: str
    timeout: float = 30.0


class Schema(pydantic.BaseModel):
    model: str
    tools: list[Inner] | None = None
    nested: Inner | None = None
    either: Inner | str | None = None


def test_a_clean_config_passes():
    refuse_unsupported({"model": "m", "tools": [{"url": "http://t"}], "nested": {"url": "http://n"}}, Schema)


def test_a_top_level_unknown_field_refuses_with_its_name():
    with pytest.raises(ConfigRefused, match="executeCodeBlocks"):
        refuse_unsupported({"model": "m", "executeCodeBlocks": True}, Schema)


def test_a_nested_unknown_field_refuses_with_its_path():
    with pytest.raises(ConfigRefused, match=r"nested\.exfiltrate"):
        refuse_unsupported({"model": "m", "nested": {"url": "http://n", "exfiltrate": True}}, Schema)
    with pytest.raises(ConfigRefused, match=r"tools\.1\.shadow"):
        refuse_unsupported({"model": "m", "tools": [{"url": "http://a"}, {"url": "http://b", "shadow": 1}]}, Schema)


def test_a_union_member_field_is_known():
    refuse_unsupported({"model": "m", "either": {"url": "http://u"}}, Schema)


def test_sub_agents_get_the_named_runtime_mismatch_refusal():
    with pytest.raises(ConfigRefused, match="Go-compiled config"):
        refuse_unsupported({"model": "m", "sub_agents": [{"name": "child"}]}, Schema)


# -- against the pinned kagent-adk, when installed --------------------


def test_the_real_agent_config_accepts_a_stock_config_and_refuses_extras():
    types = pytest.importorskip("kagent.adk.types", reason="the kagent-adk lane is not installed")
    stock = {
        "model": {"type": "openai", "model": "gpt-5.2"},
        "description": "demo",
        "instruction": "help",
    }
    refuse_unsupported(stock, types.AgentConfig)
    with pytest.raises(ConfigRefused, match="not_a_field"):
        refuse_unsupported({**stock, "not_a_field": 1}, types.AgentConfig)
    with pytest.raises(ConfigRefused, match="sub_agents"):
        refuse_unsupported({**stock, "sub_agents": []}, types.AgentConfig)


def test_a_divergent_summarizer_refuses_and_the_agent_model_passes():
    types = pytest.importorskip("kagent.adk.types", reason="the kagent-adk lane is not installed")
    from appa_kagent_adk.config_guard import refuse_divergent_summarizer

    model = {"type": "openai", "model": "gpt-5.2"}
    other = {"type": "openai", "model": "gpt-5.2-mini"}
    compaction = {"compaction_interval": 10, "overlap_size": 2}
    base = {"model": model, "description": "demo", "instruction": "help"}
    same = types.AgentConfig.model_validate(
        {**base, "context_config": {"compaction": {**compaction, "summarizer_model": model}}}
    )
    refuse_divergent_summarizer(same)
    none = types.AgentConfig.model_validate({**base, "context_config": {"compaction": compaction}})
    refuse_divergent_summarizer(none)
    divergent = types.AgentConfig.model_validate(
        {**base, "context_config": {"compaction": {**compaction, "summarizer_model": other}}}
    )
    with pytest.raises(ConfigRefused, match="summarizer"):
        refuse_divergent_summarizer(divergent)
