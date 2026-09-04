"""The image-env fill of the OpenAI reasoning effort: pure config, no lane needed."""

from appa_kagent_adk.entrypoint import REASONING_EFFORT_ENV, fill_reasoning_effort


def _config(model: dict) -> dict:
    return {"model": dict(model), "description": "a demo agent", "instruction": "help"}


def test_the_env_fills_an_unset_openai_reasoning_effort():
    config = _config({"type": "openai", "model": "gpt-5.6-luna"})
    fill_reasoning_effort(config, {REASONING_EFFORT_ENV: "none"})
    assert config["model"]["reasoning_effort"] == "none"


def test_a_value_the_crd_set_wins_over_the_env():
    config = _config({"type": "openai", "model": "gpt-5.6-luna", "reasoning_effort": "low"})
    fill_reasoning_effort(config, {REASONING_EFFORT_ENV: "none"})
    assert config["model"]["reasoning_effort"] == "low"


def test_no_env_leaves_the_rendered_config_untouched():
    config = _config({"type": "openai", "model": "gpt-5.6-luna"})
    fill_reasoning_effort(config, {})
    assert "reasoning_effort" not in config["model"]


def test_a_model_of_another_type_is_untouched():
    config = _config({"type": "anthropic", "model": "claude-sonnet-5"})
    fill_reasoning_effort(config, {REASONING_EFFORT_ENV: "none"})
    assert "reasoning_effort" not in config["model"]
