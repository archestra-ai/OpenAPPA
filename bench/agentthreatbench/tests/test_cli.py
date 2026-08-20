import pytest

from appa_agentthreatbench.cli import build_parser


def test_smoke_does_not_advertise_ignored_filters() -> None:
    parser = build_parser()

    smoke = parser.parse_args(["smoke"])
    run = parser.parse_args(["run"])

    assert not hasattr(smoke, "task_type")
    assert not hasattr(smoke, "arms")
    assert run.task_type == "all"
    assert run.arms == "all"

    with pytest.raises(SystemExit):
        parser.parse_args(["smoke", "--arms", "fides"])
