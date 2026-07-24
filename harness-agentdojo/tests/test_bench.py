import pytest

from appa_dojo.bench import grouped_means, policy_name_for


@pytest.mark.parametrize(
    ("defense", "expected"),
    [
        ("none", "slack-open"),
        ("appa-open", "slack-open"),
        ("appa-practical", "slack-practical"),
        ("appa-complete", "slack-complete"),
    ],
)
def test_slack_defense_selects_its_named_policy(defense: str, expected: str) -> None:
    assert policy_name_for("slack", defense) == expected


def test_grouped_means_preserve_each_eval_dimension() -> None:
    results = {
        ("user_task_0", "injection_task_1"): True,
        ("user_task_0", "injection_task_2"): False,
        ("user_task_1", "injection_task_1"): False,
        ("user_task_1", "injection_task_2"): False,
    }

    assert grouped_means(results, 0) == {
        "user_task_0": 0.5,
        "user_task_1": 0.0,
    }
    assert grouped_means(results, 1) == {
        "injection_task_1": 0.5,
        "injection_task_2": 0.0,
    }
