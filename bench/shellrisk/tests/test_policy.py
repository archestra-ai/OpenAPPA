import tomllib

from appa_shellrisk.policy import (
    RISK_MARK,
    ModelProfile,
    annotation_policy,
)


def profile() -> ModelProfile:
    return ModelProfile("openai", "test/model", "https://model.test/v1", "TOKEN", 1000, 2)


def test_annotation_arm_exposes_only_the_shell_risk_projection() -> None:
    config = tomllib.loads(annotation_policy(profile()))
    policy = config["policy"]
    annotator = policy["annotator"][0]

    assert policy["trust_chain"] == ["trusted"]
    assert annotator["ranks"] == ["trusted"]
    assert annotator["audiences"] == []
    assert annotator["marks"] == [RISK_MARK]
    assert annotator["effects"] == []
    assert policy["tool"][0]["annotator"] == annotator["name"]
    assert policy["authority"][0]["permits"] == {"attention": [RISK_MARK]}
    assert "authorities" not in config["externals"]
    assert config["externals"]["llm"]["token_env"] == "APPA_SHELLRISK_TOKEN"
