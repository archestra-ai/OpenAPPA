"""The refusal set, against a local schema and the real AgentConfig."""

from typing import Literal

import pydantic
import pytest

from appa_kagent_adk.config_guard import ConfigRefused, refuse_unsupported
from appa_kagent_adk.wire import RUNTIME_TOOLS


class Inner(pydantic.BaseModel):
    url: str
    timeout: float = 30.0


class Schema(pydantic.BaseModel):
    model: str
    tools: list[Inner] | None = None
    nested: Inner | None = None
    either: Inner | str | None = None
    headers: dict[str, str] | None = None


class Aliased(pydantic.BaseModel):
    plain: int = pydantic.Field(alias="plain_alias")
    either: int | None = pydantic.Field(default=None, validation_alias=pydantic.AliasChoices("either", "or_else"))


class PathRead(pydantic.BaseModel):
    deep: int | None = pydantic.Field(default=None, validation_alias=pydantic.AliasPath("outer", "inner"))
    mixed: int | None = pydantic.Field(
        default=None, validation_alias=pydantic.AliasChoices("mixed", pydantic.AliasPath("outer", "mixed"))
    )


class ByName(pydantic.BaseModel):
    model_config = pydantic.ConfigDict(validate_by_name=True)
    plain: int = pydantic.Field(alias="plain_alias")


class Left(pydantic.BaseModel):
    kind: Literal["left"]
    only_left: int | None = None


class Right(pydantic.BaseModel):
    kind: Literal["right"]
    only_right: int | None = None


class Pick(pydantic.BaseModel):
    which: Left | Right = pydantic.Field(discriminator="kind")


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


def test_a_plain_dict_field_carries_data_not_fields():
    refuse_unsupported({"model": "m", "headers": {"x-anything": "v"}}, Schema)


def test_every_alias_spelling_is_known():
    refuse_unsupported({"plain_alias": 1, "either": 2}, Aliased)
    refuse_unsupported({"plain_alias": 1, "or_else": 2}, Aliased)
    with pytest.raises(ConfigRefused, match="nope"):
        refuse_unsupported({"plain_alias": 1, "nope": 0}, Aliased)


def test_a_key_read_through_an_alias_path_is_refused_as_unknown():
    """pydantic reads the key, and the guard still refuses it: the walk cannot pair the path with a model."""
    assert PathRead.model_validate({"outer": {"inner": 3}}).deep == 3
    with pytest.raises(ConfigRefused, match=r"gate: outer$"):
        refuse_unsupported({"outer": {"inner": 3}}, PathRead)
    refuse_unsupported({"mixed": 1}, PathRead)
    assert PathRead.model_validate({"outer": {"mixed": 1}}).mixed == 1
    with pytest.raises(ConfigRefused, match=r"gate: outer$"):
        refuse_unsupported({"outer": {"mixed": 1}}, PathRead)


def test_the_bare_name_of_an_aliased_field_counts_only_under_validate_by_name():
    with pytest.raises(ConfigRefused, match=r"gate: plain$"):
        refuse_unsupported({"plain_alias": 1, "plain": 2}, Aliased)
    refuse_unsupported({"plain": 1}, ByName)


def test_a_key_of_a_sibling_union_member_is_unknown():
    refuse_unsupported({"which": {"kind": "left", "only_left": 1}}, Pick)
    with pytest.raises(ConfigRefused, match=r"which\.only_left"):
        refuse_unsupported({"which": {"kind": "right", "only_left": 1}}, Pick)


def test_an_invalid_config_surfaces_the_validation_error():
    with pytest.raises(pydantic.ValidationError):
        refuse_unsupported({"tools": [], "nope": 1}, Schema)


def test_sub_agents_get_the_named_runtime_mismatch_refusal():
    with pytest.raises(ConfigRefused, match="Go-compiled config"):
        refuse_unsupported({"model": "m", "sub_agents": [{"name": "child"}]}, Schema)


@pytest.mark.parametrize("reserved", ["appa_return", *RUNTIME_TOOLS])
def test_a_declared_tool_under_an_appa_owned_name_refuses(reserved):
    """APPA owns these names: the return gate a child scope stops
    through, and the runtime tools the entrypoint appends after this
    guard runs. The refusal reads the raw config, so it names the
    collision before validation."""
    server = {"params": {"url": "http://mcp"}, "tools": ["read_ledger", reserved]}
    with pytest.raises(ConfigRefused, match=r"http_tools\.0\.tools\.1"):
        refuse_unsupported({"model": "m", "http_tools": [server]}, Schema)
    with pytest.raises(ConfigRefused, match=r"sse_tools\.0\.tools\.0"):
        refuse_unsupported({"model": "m", "sse_tools": [{"tools": [reserved]}]}, Schema)
    with pytest.raises(ConfigRefused, match=r"remote_agents\.0\.name"):
        refuse_unsupported({"model": "m", "remote_agents": [{"name": reserved}]}, Schema)


# -- against the pinned kagent-adk, when installed --------------------

STOCK = {
    "model": {"type": "openai", "model": "gpt-5.2"},
    "description": "demo",
    "instruction": "help",
}


def test_the_real_agent_config_accepts_a_stock_config_and_refuses_extras():
    types = pytest.importorskip("kagent.adk.types", reason="the kagent-adk lane is not installed")
    refuse_unsupported(STOCK, types.AgentConfig)
    with pytest.raises(ConfigRefused, match="not_a_field"):
        refuse_unsupported({**STOCK, "not_a_field": 1}, types.AgentConfig)
    with pytest.raises(ConfigRefused, match="sub_agents"):
        refuse_unsupported({**STOCK, "sub_agents": []}, types.AgentConfig)


def test_a_key_of_a_sibling_model_variant_is_refused_with_its_path():
    types = pytest.importorskip("kagent.adk.types", reason="the kagent-adk lane is not installed")
    with pytest.raises(ConfigRefused, match=r"model\.region"):
        refuse_unsupported({**STOCK, "model": {**STOCK["model"], "region": "us-east-1"}}, types.AgentConfig)
    anthropic = {"type": "anthropic", "model": "claude-sonnet-4-5", "reasoning_effort": "low"}
    with pytest.raises(ConfigRefused, match=r"model\.reasoning_effort"):
        refuse_unsupported({**STOCK, "model": anthropic}, types.AgentConfig)
    summarizer = {**STOCK["model"], "region": "us-east-1"}
    compaction = {"compaction_interval": 10, "overlap_size": 2, "summarizer_model": summarizer}
    with pytest.raises(ConfigRefused, match=r"context_config\.compaction\.summarizer_model\.region"):
        refuse_unsupported({**STOCK, "context_config": {"compaction": compaction}}, types.AgentConfig)


def test_both_tls_spellings_on_the_model_pass_and_reach_the_field():
    types = pytest.importorskip("kagent.adk.types", reason="the kagent-adk lane is not installed")
    for spelling in ("tls_disable_verify", "tls_insecure_skip_verify"):
        config = {**STOCK, "model": {**STOCK["model"], spelling: True}}
        refuse_unsupported(config, types.AgentConfig)
        assert types.AgentConfig.model_validate(config).model.tls_disable_verify is True


def test_nested_stock_fields_pass_and_nested_extras_name_their_path():
    types = pytest.importorskip("kagent.adk.types", reason="the kagent-adk lane is not installed")
    http_tools = [{"params": {"url": "http://mcp", "headers": {"x-any": "v"}}, "tools": ["a"]}]
    refuse_unsupported({**STOCK, "http_tools": http_tools}, types.AgentConfig)
    memory = {"ttl_days": 1, "embedding": {"model": "m", "provider": "p", "bogus": 1}}
    with pytest.raises(ConfigRefused, match=r"memory\.embedding\.bogus"):
        refuse_unsupported({**STOCK, "memory": memory}, types.AgentConfig)


def test_the_tls_keys_inside_mcp_params_pass_and_reach_the_tool_config():
    """kagent lifts the TLS keys from params to the tool config, so the guard reads them there."""
    types = pytest.importorskip("kagent.adk.types", reason="the kagent-adk lane is not installed")
    for kind in ("http_tools", "sse_tools"):
        params = {"url": "http://mcp", "tls_insecure_skip_verify": True, "tls_ca_cert_path": "/ca.pem"}
        config = {**STOCK, kind: [{"params": params}]}
        refuse_unsupported(config, types.AgentConfig)
        (tool_config,) = getattr(types.AgentConfig.model_validate(config), kind)
        assert (tool_config.tls_insecure_skip_verify, tool_config.tls_ca_cert_path) == (True, "/ca.pem")
    other = [{"params": {"url": "http://mcp", "tls_bogus": True}}]
    with pytest.raises(ConfigRefused, match=r"http_tools\.0\.params\.tls_bogus"):
        refuse_unsupported({**STOCK, "http_tools": other}, types.AgentConfig)


@pytest.mark.parametrize("reserved", ["appa_return", *RUNTIME_TOOLS])
def test_the_real_agent_config_refuses_an_appa_owned_tool_name(reserved):
    types = pytest.importorskip("kagent.adk.types", reason="the kagent-adk lane is not installed")
    filtered = [{"params": {"url": "http://mcp"}, "tools": [reserved]}]
    with pytest.raises(ConfigRefused, match=reserved):
        refuse_unsupported({**STOCK, "http_tools": filtered}, types.AgentConfig)
    with pytest.raises(ConfigRefused, match=reserved):
        refuse_unsupported({**STOCK, "remote_agents": [{"name": reserved, "url": "http://child"}]}, types.AgentConfig)
    # The same shapes under any other tool name start.
    refuse_unsupported(
        {**STOCK, "http_tools": [{"params": {"url": "http://mcp"}, "tools": ["read_ledger"]}]}, types.AgentConfig
    )
    refuse_unsupported({**STOCK, "remote_agents": [{"name": "log-analyst", "url": "http://child"}]}, types.AgentConfig)


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
